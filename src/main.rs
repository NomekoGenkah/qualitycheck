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
use qualitycheck::error::{ConfigError, QualityCheckError, ScanError};
use qualitycheck::git::{find_repo_root, get_changed_files};
use qualitycheck::jev_client::JevClient;
use qualitycheck::output::{
    diff_runs, filter_gaps, list_saved_runs, load_saved_run, persist_run_result,
    print_diff_report, print_file_evaluation, print_preview_result, print_scan_result, OutputFormat,
};
use qualitycheck::pipeline::run_preview_pipeline;
use qualitycheck::profile::{list_available_profiles, load_profile, load_profiles_by_names};
use qualitycheck::scorer::run_scan_pipeline;
use qualitycheck::walker::{collect_files, WalkerOptions};

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
    let target_path = args.path.clone();
    if !target_path.exists() {
        return Err(ScanError::PathNotFound(target_path).into());
    }

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
    };

    let files = collect_files(&target_path, &walker_opts)?;
    if files.is_empty() {
        eprintln!("No candidate files found to scan in '{}'.", target_path.display());
        return Ok(0);
    }

    let project_root = find_repo_root(&target_path)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let output_format = match args.format.to_lowercase().as_str() {
        "json" => OutputFormat::Json,
        _ => OutputFormat::Table,
    };

    if args.preview {
        let preview = run_preview_pipeline(&target_path, &files, &profiles, &project_root)?;
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
        target_path.display(),
        files.len(),
        ignore_status,
        args.concurrency
    );

    let scan_result = run_scan_pipeline(
        &target_path,
        &files,
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
    let target_path = args.path.clone();
    let project_root = find_repo_root(&target_path)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    let changed_files = get_changed_files(&project_root, args.base.as_deref())?;

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

    if args.preview {
        let preview = run_preview_pipeline(&project_root, &changed_files, &profiles, &project_root)?;
        print_preview_result(&preview, output_format, args.no_color);
        return Ok(0);
    }

    ensure_configured_or_init(args.no_color)?;

    let api_key = resolve_api_key(None)?;
    let jev_url = resolve_jev_url();
    let client = Arc::new(JevClient::new(api_key, jev_url));

    eprintln!(
        "Patch scan ({} changed files vs {})...",
        changed_files.len(),
        args.base.as_deref().unwrap_or("working tree")
    );

    let scan_result = run_scan_pipeline(
        &project_root,
        &changed_files,
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
