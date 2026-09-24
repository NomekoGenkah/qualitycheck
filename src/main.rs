use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use is_terminal::IsTerminal;
use serde_json::json;

use qualitycheck::cli::{
    Cli, Commands, DiffArgs, FileArgs, GapsArgs, InitArgs, PatchArgs, ProfilesCommand, RunsCommand,
    ScanArgs,
};
use qualitycheck::config::{
    execute_init, get_config_file_path, install_default_profiles, resolve_api_key,
    resolve_jev_url,
};
use qualitycheck::context::{attach_related_files, base_side_overrides};
use qualitycheck::error::{ConfigError, QualityCheckError, ScanError};
use qualitycheck::git::{find_repo_root, get_changed_files, ChangedFile};
use qualitycheck::jev_client::JevClient;
use qualitycheck::output::{
    diff_runs, filter_gaps, list_saved_runs, load_saved_run, persist_run_result,
    print_diff_report, print_file_evaluation, print_patch_delta_report, print_preview_result,
    print_scan_result, OutputFormat, PatchDeltaReport,
};
use qualitycheck::pipeline::{
    run_preview_pipeline_on_inputs, run_scan_pipeline_on_inputs, ScanInput,
};
use qualitycheck::profile::{list_available_profiles, load_profile, load_profiles_by_names};
use qualitycheck::scorer::{find_metric_regressions, find_regressions, run_scan_pipeline, RunUsage};
use qualitycheck::walker::{
    collect_files_from_targets, filter_candidate_files, is_content_eligible, WalkerOptions,
};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cli = Cli::parse();

    let exit_code = match run(cli, &args).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {}", err);
            err.exit_code()
        }
    };

    std::process::exit(exit_code);
}

async fn run(cli: Cli, _raw_args: &[String]) -> Result<i32, QualityCheckError> {
    // If no explicit subcommand was passed, check if first arg was a path for scan or default scan
    let command = match cli.command {
        Some(cmd) => cmd,
        None => Commands::Scan(cli.scan_args),
    };

    match command {
        Commands::Init(args) => handle_init(args),
        Commands::Profiles { command } => handle_profiles(command),
        Commands::Describe => handle_describe(),
        Commands::Scan(args) => handle_scan(args).await,
        Commands::Patch(args) => handle_patch(args).await,
        Commands::Gaps(args) => handle_gaps(args),
        Commands::File(args) => handle_file(args).await,
        Commands::Diff(args) => handle_diff(args),
        Commands::Runs { command } => handle_runs(command),
    }
}

fn handle_init(args: InitArgs) -> Result<i32, QualityCheckError> {
    execute_init(args.api_key, args.non_interactive, true)?;
    Ok(0)
}

fn handle_profiles(cmd: ProfilesCommand) -> Result<i32, QualityCheckError> {
    match cmd {
        ProfilesCommand::List => {
            let profiles = list_available_profiles();
            if profiles.is_empty() {
                println!("No profiles found.");
                return Ok(0);
            }

            println!("{:<15} {:<10} {:<10} {:<8} DESCRIPTION", "NAME", "FAIL_BELOW", "METRICS", "SOURCE");
            println!("{:-<15} {:-<10} {:-<10} {:-<8} {:-<35}", "", "", "", "", "");
            for p in profiles {
                let source = if p.is_custom { "custom" } else { "builtin" };
                println!(
                    "{:<15} {:<10.1} {:<10} {:<8} {}",
                    p.name, p.fail_below, p.metric_count, source, p.description
                );
            }
            Ok(0)
        }
        ProfilesCommand::Show { name } => {
            let profile = load_profile(&name)?;
            let json = serde_json::to_string_pretty(&profile)
                .map_err(|e| QualityCheckError::Usage(format!("Failed to serialize profile: {e}")))?;
            println!("{}", json);
            Ok(0)
        }
    }
}

fn handle_describe() -> Result<i32, QualityCheckError> {
    let profiles = list_available_profiles();
    let profile_entries: Vec<_> = profiles
        .into_iter()
        .map(|p| {
            let full_profile = load_profile(&p.name).ok();
            let metric_ids = full_profile
                .map(|fp| fp.metrics.into_iter().map(|m| m.id).collect::<Vec<_>>())
                .unwrap_or_default();

            json!({
                "name": p.name,
                "description": p.description,
                "fail_below": p.fail_below,
                "metrics": metric_ids,
            })
        })
        .collect();

    let manifest = json!({
        "commands": ["scan", "patch", "gaps", "file", "diff", "runs", "profiles", "init", "describe"],
        "profiles": profile_entries,
    });

    println!("{}", serde_json::to_string_pretty(&manifest).unwrap());
    Ok(0)
}

async fn handle_scan(args: ScanArgs) -> Result<i32, QualityCheckError> {
    let targets = args.paths;
    if let Some(missing) = targets.iter().find(|p| !p.exists()) {
        return Err(ScanError::PathNotFound(missing.clone()).into());
    }
    let targets_label = targets
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let profile_names: Vec<&str> = args
        .profile
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if profile_names.is_empty() {
        return Err(QualityCheckError::Usage("No profiles specified".to_string()));
    }

    let profiles = load_profiles_by_names(&profile_names)?;

    let walker_opts = WalkerOptions {
        no_ignore: args.no_ignore,
        include: args.include,
        exclude: args.exclude,
        max_file_size_kb: args.max_file_size,
        all_files: args.all_files,
    };

    let collected = collect_files_from_targets(&targets, &walker_opts)?;
    report_skipped_non_source(collected.skipped_non_source);
    let files = collected.files;
    if files.is_empty() {
        eprintln!("No candidate files found to scan in '{}'.", targets_label);
        return Ok(0);
    }

    let project_root = find_repo_root(&targets[0])
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // A single target keeps file paths relative to it; several are shown relative to the repo.
    let target_path = match targets.as_slice() {
        [single] => single.clone(),
        _ => project_root.clone(),
    };

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    let mut inputs: Vec<ScanInput> = files.into_iter().map(ScanInput::from_disk).collect();
    if args.context {
        attach_related_files(&mut inputs, &project_root, &HashMap::new(), args.max_file_size);
    }

    if args.preview {
        let mut preview = run_preview_pipeline_on_inputs(&target_path, &inputs, &profiles, &project_root, args.full)?;
        preview.skipped_non_source_files = collected.skipped_non_source;
        print_preview_result(&preview, output_format, args.no_color);
        return Ok(0);
    }

    ensure_configured_or_init(args.no_color)?;

    let api_key = resolve_api_key(None)?;
    let jev_url = resolve_jev_url();
    let client = Arc::new(JevClient::new(api_key, jev_url));

    let ignore_status = if args.no_ignore {
        ".gitignore ignored"
    } else {
        ".gitignore respected"
    };

    eprintln!(
        "Scanning {} ({} files, {}, concurrency {})...",
        targets_label,
        inputs.len(),
        ignore_status,
        args.concurrency
    );

    let scan_result = run_scan_pipeline_on_inputs(
        &target_path,
        inputs,
        &profiles,
        client,
        args.concurrency,
        &project_root,
    )
    .await?;

    let saved_path = persist_run_result(
        &scan_result,
        args.save.as_deref(),
        args.no_persist,
        &project_root,
    )
    .ok()
    .flatten();

    print_scan_result(&scan_result, output_format, args.no_color, saved_path.as_deref());

    if args.strict && !scan_result.passed {
        return Ok(1);
    }

    Ok(0)
}

async fn handle_patch(args: PatchArgs) -> Result<i32, QualityCheckError> {
    if args.fail_on_regression.is_some_and(|max_drop| max_drop < 0.0) {
        return Err(QualityCheckError::Usage(
            "--fail-on-regression must be zero or positive".to_string(),
        ));
    }
    if args.fail_on_metric_regression.is_some_and(|max_drop| max_drop < 0.0) {
        return Err(QualityCheckError::Usage(
            "--fail-on-metric-regression must be zero or positive".to_string(),
        ));
    }
    let compare_base =
        args.delta || args.fail_on_regression.is_some() || args.fail_on_metric_regression.is_some();

    let target_path = args.path.clone();
    let project_root = find_repo_root(&target_path)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let changes = get_changed_files(&project_root, args.base.as_deref())?;

    let walker_opts = WalkerOptions {
        no_ignore: false,
        include: args.include,
        exclude: args.exclude,
        max_file_size_kb: args.max_file_size,
        all_files: args.all_files,
    };
    let changed_paths: Vec<PathBuf> = changes.files.iter().map(|f| f.path.clone()).collect();
    let collected = filter_candidate_files(&changed_paths, &project_root, &walker_opts)?;
    report_skipped_non_source(collected.skipped_non_source);
    let skipped_non_source = collected.skipped_non_source;
    let eligible: HashSet<PathBuf> = collected.files.into_iter().collect();
    let changed_files: Vec<ChangedFile> = changes
        .files
        .into_iter()
        .filter(|f| eligible.contains(&f.path))
        .collect();

    if changed_files.is_empty() {
        eprintln!("No changed files detected to scan.");
        return Ok(0);
    }

    let profile_names: Vec<&str> = args
        .profile
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    let profiles = load_profiles_by_names(&profile_names)?;

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    let mut head_inputs: Vec<ScanInput> = changed_files
        .iter()
        .map(|f| ScanInput::from_disk(f.path.clone()))
        .collect();
    if args.context {
        attach_related_files(&mut head_inputs, &project_root, &HashMap::new(), args.max_file_size);
    }
    let base_overrides = if compare_base && args.context {
        base_side_overrides(&changed_files, &project_root, args.max_file_size)
    } else {
        HashMap::new()
    };
    // Each file's base version is scored under its current path so the two sides line up; a
    // base that is binary, empty, or oversized counts as no base.
    let mut base_inputs: Vec<ScanInput> = if compare_base {
        changed_files
            .into_iter()
            .filter_map(|f| {
                let content = f
                    .base_content
                    .filter(|bytes| is_content_eligible(bytes, args.max_file_size))?;
                Some(ScanInput::in_memory(f.path, content))
            })
            .collect()
    } else {
        Vec::new()
    };
    if args.context {
        // The base side sees the base versions of related files, so score changes reflect the
        // code rather than a difference in context.
        attach_related_files(&mut base_inputs, &project_root, &base_overrides, args.max_file_size);
    }

    if args.preview {
        // Base versions are labelled for display only; their content is already in memory.
        let base_label = format!(" (at {})", changes.base);
        let mut inputs = head_inputs;
        inputs.extend(base_inputs.into_iter().map(|input| {
            let mut labelled = input.path.into_os_string();
            labelled.push(&base_label);
            ScanInput {
                path: PathBuf::from(labelled),
                ..input
            }
        }));
        let mut preview = run_preview_pipeline_on_inputs(&project_root, &inputs, &profiles, &project_root, args.full)?;
        preview.skipped_non_source_files = skipped_non_source;
        print_preview_result(&preview, output_format, args.no_color);
        return Ok(0);
    }

    ensure_configured_or_init(args.no_color)?;

    let api_key = resolve_api_key(None)?;
    let jev_url = resolve_jev_url();
    let client = Arc::new(JevClient::new(api_key, jev_url));

    let base_note = if compare_base {
        format!(", plus {} base versions", base_inputs.len())
    } else {
        String::new()
    };
    eprintln!(
        "Patch scan ({} changed files vs {}{})...",
        head_inputs.len(),
        changes.base,
        base_note
    );

    let head_result = run_scan_pipeline_on_inputs(
        &project_root,
        head_inputs,
        &profiles,
        Arc::clone(&client),
        args.concurrency,
        &project_root,
    )
    .await?;

    let saved_path = persist_run_result(
        &head_result,
        args.save.as_deref(),
        args.no_persist,
        &project_root,
    )
    .ok()
    .flatten();

    let strict_failed = args.strict && !head_result.passed;

    if !compare_base {
        print_scan_result(&head_result, output_format, args.no_color, saved_path.as_deref());
        return Ok(if strict_failed { 1 } else { 0 });
    }

    let base_result = run_scan_pipeline_on_inputs(
        &project_root,
        base_inputs,
        &profiles,
        client,
        args.concurrency,
        &project_root,
    )
    .await?;

    let regressions = args
        .fail_on_regression
        .map(|max_drop| find_regressions(&base_result, &head_result, max_drop))
        .unwrap_or_default();
    let metric_regressions = args
        .fail_on_metric_regression
        .map(|max_drop| find_metric_regressions(&base_result, &head_result, max_drop))
        .unwrap_or_default();
    let failed = strict_failed || !regressions.is_empty() || !metric_regressions.is_empty();

    let report = PatchDeltaReport {
        base: changes.base,
        head_run_id: head_result.run_id.clone(),
        max_regression: args.fail_on_regression,
        max_metric_regression: args.fail_on_metric_regression,
        passed: !failed,
        regressions,
        metric_regressions,
        file_diffs: diff_runs(&base_result, &head_result).file_diffs,
        usage: combine_usage(&base_result.usage, &head_result.usage),
    };
    print_patch_delta_report(&report, output_format, args.no_color, saved_path.as_deref());

    Ok(if failed { 1 } else { 0 })
}

fn report_skipped_non_source(skipped: usize) {
    if skipped > 0 {
        eprintln!(
            "Skipped {} file(s) that aren't source code (docs, config, vendored, minified, or generated); use --all-files to include them.",
            skipped
        );
    }
}

fn combine_usage(a: &RunUsage, b: &RunUsage) -> RunUsage {
    RunUsage {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        total_tokens: a.total_tokens + b.total_tokens,
        estimated_cost_usd: a.estimated_cost_usd + b.estimated_cost_usd,
    }
}

fn handle_gaps(args: GapsArgs) -> Result<i32, QualityCheckError> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let run_id = args.run.as_deref().unwrap_or("latest");

    let run_result = load_saved_run(run_id, &project_root).map_err(|e| {
        QualityCheckError::Usage(format!("Failed to load run '{}': {}", run_id, e))
    })?;

    let gaps_result = filter_gaps(&run_result);

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    if gaps_result.files.is_empty() {
        if output_format == OutputFormat::Json {
            println!("{}", serde_json::to_string_pretty(&gaps_result).unwrap());
        } else {
            println!("No quality gaps found for run '{}'. All metrics passed!", run_id);
        }
        return Ok(0);
    }

    print_scan_result(&gaps_result, output_format, args.no_color, None);
    Ok(0)
}

async fn handle_file(args: FileArgs) -> Result<i32, QualityCheckError> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    // Try loading from specified run or latest run
    let target_file_str = args.path.to_string_lossy();
    let run_id = args.run.as_deref().unwrap_or("latest");

    if let Ok(run) = load_saved_run(run_id, &project_root)
        && let Some(file_eval) = run.files.iter().find(|f| {
            f.path == args.path
                || f.relative_path == target_file_str
                || target_file_str.ends_with(&f.relative_path)
        }) {
            print_file_evaluation(file_eval, output_format, args.no_color);
            return Ok(0);
        }

    // If not found in run, re-score file on demand
    ensure_configured_or_init(args.no_color)?;
    let profiles = load_profiles_by_names(&["quality"])?;
    let api_key = resolve_api_key(None)?;
    let jev_url = resolve_jev_url();
    let client = Arc::new(JevClient::new(api_key, jev_url));

    let scan_result = run_scan_pipeline(
        &project_root,
        std::slice::from_ref(&args.path),
        &profiles,
        client,
        1,
        &project_root,
    )
    .await?;

    if let Some(file_eval) = scan_result.files.first() {
        print_file_evaluation(file_eval, output_format, args.no_color);
    } else {
        return Err(ScanError::PathNotFound(args.path).into());
    }

    Ok(0)
}

fn handle_diff(args: DiffArgs) -> Result<i32, QualityCheckError> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let run_a = load_saved_run(&args.run_a, &project_root).map_err(|e| {
        QualityCheckError::Usage(format!("Failed to load run-a '{}': {}", args.run_a, e))
    })?;

    let run_b = load_saved_run(&args.run_b, &project_root).map_err(|e| {
        QualityCheckError::Usage(format!("Failed to load run-b '{}': {}", args.run_b, e))
    })?;

    if run_a.scoring_version != run_b.scoring_version {
        eprintln!(
            "Warning: runs were scored under different scoring versions ({} vs {}); deltas \
             reflect scoring changes, not only code changes. Re-scan the older revision to compare.",
            run_a.scoring_version, run_b.scoring_version
        );
    }

    let diff_report = diff_runs(&run_a, &run_b);

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    print_diff_report(&diff_report, output_format, args.no_color);
    Ok(0)
}

fn handle_runs(cmd: RunsCommand) -> Result<i32, QualityCheckError> {
    match cmd {
        RunsCommand::List => {
            let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let runs = list_saved_runs(&project_root)?;

            if runs.is_empty() {
                println!("No saved runs found in .qualitycheck/runs/");
                return Ok(0);
            }

            println!("{:<25} {:<25} {:<8} STATUS", "RUN ID", "TIMESTAMP", "FILES");
            println!("{:-<25} {:-<25} {:-<8} {:-<10}", "", "", "", "");

            for run in runs {
                let status = if run.passed { "PASS" } else { "FAIL" };
                let ts_str = run.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();
                println!("{:<25} {:<25} {:<8} {}", run.run_id, ts_str, run.file_count, status);
            }
            Ok(0)
        }
    }
}

fn ensure_configured_or_init(_no_color: bool) -> Result<(), QualityCheckError> {
    if resolve_api_key(None).is_ok() {
        let _ = install_default_profiles();
        return Ok(());
    }

    let config_path = get_config_file_path().unwrap_or_default();
    if !config_path.exists() {
        // If stdin is a TTY and interactive, trigger init automatically!
        if std::io::stdin().is_terminal() {
            eprintln!("No configuration found. Running initial setup wizard...\n");
            execute_init(None, false, true)?;
            return Ok(());
        }
    }

    Err(ConfigError::MissingApiKey.into())
}
