use std::io;
use std::path::Path;

use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};

use crate::cache::RawMetricValue;
use crate::pipeline::ScanPreviewResult;
use crate::profile::MetricType;
use crate::scorer::{
    round_to_tenth, FileEvaluation, MetricEvaluation, Regression, RegressionKind, RunUsage,
    ScanRunResult,
};

pub use crate::storage::{get_runs_dir, list_saved_runs, load_saved_run, persist_run_result, RunListItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
}

/// Scores are `None` on the old side when the file, profile, or metric didn't exist there.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricDiff {
    pub metric_id: String,
    pub old_score: Option<f64>,
    pub new_score: f64,
    pub delta: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDiff {
    pub profile_name: String,
    pub old_composite: Option<f64>,
    pub new_composite: f64,
    pub delta: Option<f64>,
    pub metric_diffs: Vec<MetricDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub relative_path: String,
    /// The file has no counterpart in the old run.
    pub is_new: bool,
    pub profile_diffs: Vec<ProfileDiff>,
}

/// `patch --delta`: every changed file scored at the base and in the working tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchDeltaReport {
    pub base: String,
    pub head_run_id: String,
    /// `--fail-on-regression` allowance, when gating was requested.
    pub max_regression: Option<f64>,
    pub passed: bool,
    pub regressions: Vec<Regression>,
    pub file_diffs: Vec<FileDiff>,
    /// Tokens for both sides combined.
    pub usage: RunUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunDiffReport {
    pub old_run_id: String,
    pub new_run_id: String,
    pub file_diffs: Vec<FileDiff>,
}

pub fn print_scan_result(
    result: &ScanRunResult,
    format: OutputFormat,
    no_color: bool,
    saved_path: Option<&Path>,
) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let colors_enabled = !no_color && io::stdout().is_terminal();

            for file in &result.files {
                println!("{}", file.relative_path);
                if !file.context_files.is_empty() {
                    println!("  context: {}", file.context_files.join(", "));
                }

                for profile in &file.profiles {
                    for metric in &profile.metrics {
                        let val_str = format_metric_display(metric);
                        let conf_pct = (metric.confidence * 100.0).round() as u64;

                        let metric_line = format!(
                            "  [{}] {}: {} ({}% conf.){}",
                            profile.profile_name,
                            metric.metric_id,
                            val_str,
                            conf_pct,
                            excluded_suffix(metric, profile.min_confidence)
                        );

                        let colored_line = if colors_enabled {
                            if metric.excluded_low_confidence || metric.not_applicable {
                                colorize(&metric_line, "90")
                            } else if metric.normalized_score >= 4.0 {
                                colorize(&metric_line, "32")
                            } else if metric.normalized_score >= 3.0 {
                                colorize(&metric_line, "33")
                            } else {
                                colorize(&metric_line, "31")
                            }
                        } else {
                            metric_line
                        };

                        println!("{}", colored_line);
                    }
                }

                let mut composite_parts = Vec::new();
                for profile in &file.profiles {
                    let score_str = if profile.inconclusive {
                        format!("{} INCONCLUSIVE", profile.profile_name)
                    } else {
                        format!("{} {:.1}/5", profile.profile_name, profile.composite_score)
                    };
                    let part = if colors_enabled {
                        if profile.inconclusive {
                            colorize(&score_str, "33")
                        } else if profile.passed {
                            colorize(&score_str, "32")
                        } else {
                            colorize(&score_str, "31;1")
                        }
                    } else {
                        score_str
                    };
                    composite_parts.push(part);
                }

                let separator = if colors_enabled { " · " } else { " | " };
                println!("  Composite score: {}", composite_parts.join(separator));
                println!();
            }

            let token_line = if result.cached_files == result.total_files && result.total_files > 0 {
                format!(
                    "Tokens consumed: 0 (all {} files served from cache) · Run cost: $0.00000",
                    result.total_files
                )
            } else {
                format!(
                    "Tokens consumed: {} input · {} output · Est. cost: ${:.5} ({} files served from cache)",
                    result.usage.input_tokens,
                    result.usage.output_tokens,
                    result.usage.estimated_cost_usd,
                    result.cached_files
                )
            };

            if colors_enabled {
                println!("{}", colorize(&token_line, "36"));
            } else {
                println!("{}", token_line);
            }

            if let Some(path) = saved_path {
                let msg = format!("✔ Full report saved to {}", path.display());
                if colors_enabled {
                    println!("{}", colorize(&msg, "32"));
                } else {
                    println!("{}", msg);
                }
            }
        }
    }
}

pub fn print_file_evaluation(file: &FileEvaluation, format: OutputFormat, no_color: bool) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(file).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let colors_enabled = !no_color && io::stdout().is_terminal();
            println!("{}", file.relative_path);
            if let Some(u) = &file.usage {
                let cache_status = if file.served_from_cache { " (served from cache)" } else { "" };
                println!("Tokens: {} input · {} output{}", u.input_tokens, u.output_tokens, cache_status);
            }

            for profile in &file.profiles {
                println!(
                    "--- Profile: {} (fail below: {:.1}) ---",
                    profile.profile_name, profile.fail_below
                );
                for metric in &profile.metrics {
                    let val_str = format_metric_display(metric);
                    let conf_pct = (metric.confidence * 100.0).round() as u64;
                    let line = format!(
                        "  {}: {} ({}% conf.) -> normalized: {:.1}/5{}{}",
                        metric.metric_id,
                        val_str,
                        conf_pct,
                        metric.normalized_score,
                        format_distribution(metric),
                        excluded_suffix(metric, profile.min_confidence)
                    );

                    let colored_line = if colors_enabled {
                        if metric.excluded_low_confidence || metric.not_applicable {
                            colorize(&line, "90")
                        } else if metric.normalized_score >= 4.0 {
                            colorize(&line, "32")
                        } else if metric.normalized_score >= 3.0 {
                            colorize(&line, "33")
                        } else {
                            colorize(&line, "31")
                        }
                    } else {
                        line
                    };
                    println!("{}", colored_line);
                }

                let comp_str = format!("Composite score: {:.1}/5", profile.composite_score);
                let status_str = if profile.inconclusive {
                    "INCONCLUSIVE: no metric was applicable with enough confidence"
                } else if profile.passed {
                    "PASS"
                } else {
                    "FAIL"
                };
                let summary_line = format!("{} [{}]", comp_str, status_str);

                if colors_enabled {
                    if profile.inconclusive {
                        println!("{}", colorize(&summary_line, "33;1"));
                    } else if profile.passed {
                        println!("{}", colorize(&summary_line, "32;1"));
                    } else {
                        println!("{}", colorize(&summary_line, "31;1"));
                    }
                } else {
                    println!("{}", summary_line);
                }
                println!();
            }
        }
    }
}

pub fn filter_gaps(run: &ScanRunResult) -> ScanRunResult {
    let mut failing_files = Vec::new();

    for file in &run.files {
        let mut failing_profiles = Vec::new();
        for profile in &file.profiles {
            if !profile.passed {
                failing_profiles.push(profile.clone());
            } else {
                let failing_metrics: Vec<MetricEvaluation> = profile
                    .metrics
                    .iter()
                    .filter(|m| {
                        !m.excluded_low_confidence
                            && !m.not_applicable
                            && m.normalized_score < profile.fail_below
                    })
                    .cloned()
                    .collect();

                if !failing_metrics.is_empty() {
                    let mut prof_clone = profile.clone();
                    prof_clone.metrics = failing_metrics;
                    failing_profiles.push(prof_clone);
                }
            }
        }

        if !failing_profiles.is_empty() {
            let mut file_clone = file.clone();
            file_clone.profiles = failing_profiles;
            failing_files.push(file_clone);
        }
    }

    ScanRunResult {
        scoring_version: run.scoring_version,
        run_id: run.run_id.clone(),
        timestamp: run.timestamp,
        target_path: run.target_path.clone(),
        total_files: failing_files.len(),
        cached_files: run.cached_files,
        profiles_used: run.profiles_used.clone(),
        files: failing_files,
        usage: run.usage.clone(),
        passed: run.passed,
        exit_reason: run.exit_reason.clone(),
    }
}

pub fn diff_runs(run_a: &ScanRunResult, run_b: &ScanRunResult) -> RunDiffReport {
    let mut file_diffs = Vec::new();

    for file_b in &run_b.files {
        let file_a_opt = run_a.files.iter().find(|f| f.relative_path == file_b.relative_path);

        let mut profile_diffs = Vec::new();

        for prof_b in &file_b.profiles {
            let prof_a_opt = file_a_opt.and_then(|fa| {
                fa.profiles.iter().find(|p| p.profile_name == prof_b.profile_name)
            });

            let old_comp = prof_a_opt.map(|p| p.composite_score);
            let new_comp = prof_b.composite_score;
            let comp_delta = old_comp.map(|old| round_to_tenth(new_comp - old));

            let mut metric_diffs = Vec::new();
            for m_b in &prof_b.metrics {
                let m_a_opt = prof_a_opt.and_then(|pa| {
                    pa.metrics.iter().find(|m| m.metric_id == m_b.metric_id)
                });

                // Rounded to the displayed precision so old, new, and delta always agree.
                let old_m_score = m_a_opt.map(|m| round_to_tenth(m.normalized_score));
                let new_m_score = round_to_tenth(m_b.normalized_score);
                let delta = old_m_score.map(|old| round_to_tenth(new_m_score - old));

                metric_diffs.push(MetricDiff {
                    metric_id: m_b.metric_id.clone(),
                    old_score: old_m_score,
                    new_score: new_m_score,
                    delta,
                });
            }

            profile_diffs.push(ProfileDiff {
                profile_name: prof_b.profile_name.clone(),
                old_composite: old_comp,
                new_composite: new_comp,
                delta: comp_delta,
                metric_diffs,
            });
        }

        file_diffs.push(FileDiff {
            relative_path: file_b.relative_path.clone(),
            is_new: file_a_opt.is_none(),
            profile_diffs,
        });
    }

    RunDiffReport {
        old_run_id: run_a.run_id.clone(),
        new_run_id: run_b.run_id.clone(),
        file_diffs,
    }
}

pub fn print_diff_report(report: &RunDiffReport, format: OutputFormat, no_color: bool) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let colors_enabled = !no_color && io::stdout().is_terminal();
            println!("Comparing runs: {} -> {}", report.old_run_id, report.new_run_id);
            println!();

            print_file_diffs(&report.file_diffs, colors_enabled);
        }
    }
}

pub fn print_preview_result(
    preview: &ScanPreviewResult,
    format: OutputFormat,
    no_color: bool,
) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(preview).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let colors_enabled = !no_color && io::stdout().is_terminal();

            let profiles_str = preview.profiles.join(", ");
            let mode_suffix = if preview.is_full { " [FULL BREAKDOWN]" } else { "" };
            let header = format!(
                "Scan Preview: '{}' · Profiles: [{}] ({} metrics){}",
                preview.target_path, profiles_str, preview.total_metrics, mode_suffix
            );
            if colors_enabled {
                println!("{}", colorize(&header, "1;34"));
            } else {
                println!("{}", header);
            }
            println!("{}", "-".repeat(65));

            if preview.is_full || !preview.files.is_empty() {
                // Show files (either because --full was requested, or total_files <= 5)
                for file in &preview.files {
                    let status_tag = if file.served_from_cache {
                        if colors_enabled {
                            colorize("[CACHED]", "32")
                        } else {
                            "[CACHED]".to_string()
                        }
                    } else if colors_enabled {
                        colorize("[UNCACHED]", "33")
                    } else {
                        "[UNCACHED]".to_string()
                    };

                    let cost_info = if file.served_from_cache {
                        "0 tokens · $0.00000 (cache hit)".to_string()
                    } else {
                        format!(
                            "~{} est. tokens · ~${:.5}",
                            file.estimated_input_tokens, file.estimated_cost_usd
                        )
                    };

                    println!("  {} {:<40} -> {}", status_tag, file.relative_path, cost_info);
                    if !file.context_files.is_empty() {
                        println!("      context: {}", file.context_files.join(", "));
                    }
                }
                println!("{}", "-".repeat(65));
            } else if !preview.top_uncached.is_empty() {
                // Summary mode with > 5 files: show top uncached files
                println!("Top uncached files by estimated tokens:");
                for file in &preview.top_uncached {
                    let status_tag = if colors_enabled {
                        colorize("[UNCACHED]", "33")
                    } else {
                        "[UNCACHED]".to_string()
                    };
                    println!(
                        "  {} {:<40} -> ~{} est. tokens · ~${:.5}",
                        status_tag, file.relative_path, file.estimated_input_tokens, file.estimated_cost_usd
                    );
                }
                let remaining = preview.uncached_files.saturating_sub(preview.top_uncached.len());
                if remaining > 0 {
                    let cached_note = if preview.cached_files > 0 {
                        format!(" ({} cached file(s) omitted)", preview.cached_files)
                    } else {
                        String::new()
                    };
                    println!("  ... and {} more uncached file(s){}.", remaining, cached_note);
                }
                println!("{}", "-".repeat(65));
            }

            println!(
                "Total files: {} ({} uncached, {} cached)",
                preview.total_files, preview.uncached_files, preview.cached_files
            );

            let summary_tokens = format!(
                "Est. tokens: ~{} input tokens · Est. cost: ~${:.5} USD",
                preview.total_estimated_tokens, preview.total_estimated_cost_usd
            );
            if colors_enabled {
                println!("{}", colorize(&summary_tokens, "36;1"));
            } else {
                println!("{}", summary_tokens);
            }

            if !preview.is_full && preview.total_files > 5 {
                let tip = "ℹ Output summarized to preserve LLM context. Use --full (or --verbose) to list all files.";
                if colors_enabled {
                    println!("{}", colorize(tip, "33"));
                } else {
                    println!("{}", tip);
                }
            }

            let notice = "⚡ Preview mode: No API calls made. No credits deducted.";
            if colors_enabled {
                println!("{}", colorize(notice, "90"));
            } else {
                println!("{}", notice);
            }
        }
    }
}

fn format_metric_display(metric: &MetricEvaluation) -> String {
    match &metric.raw_value {
        RawMetricValue::Scale(v) => {
            let rounded = (v * 10.0).round() / 10.0;
            let display_val = if rounded.fract() == 0.0 {
                format!("{}", rounded as i64)
            } else {
                format!("{:.1}", rounded)
            };
            if let Some(r) = metric.range {
                format!("{}/{}", display_val, r[1] as i64)
            } else {
                format!("{}/5", display_val)
            }
        }
        RawMetricValue::Enum(s) => s.clone(),
        RawMetricValue::Binary(b) => b.to_string(),
    }
}

/// Per-file composite changes, plus each metric whose score moved.
fn print_file_diffs(file_diffs: &[FileDiff], colors_enabled: bool) {
    let paint = |line: String, delta: Option<f64>| match delta {
        Some(d) if colors_enabled && d > 0.0 => colorize(&line, "32"),
        Some(d) if colors_enabled && d < 0.0 => colorize(&line, "31"),
        _ => line,
    };

    for file_diff in file_diffs {
        if file_diff.is_new {
            println!("{} (new)", file_diff.relative_path);
        } else {
            println!("{}", file_diff.relative_path);
        }

        for prof_diff in &file_diff.profile_diffs {
            let line = format!(
                "  [{}] composite: {}",
                prof_diff.profile_name,
                format_score_change(prof_diff.old_composite, prof_diff.new_composite, prof_diff.delta)
            );
            println!("{}", paint(line, prof_diff.delta));

            for metric in prof_diff.metric_diffs.iter().filter(|m| m.delta.is_some_and(|d| d != 0.0)) {
                let line = format!(
                    "      {}: {}",
                    metric.metric_id,
                    format_score_change(metric.old_score, metric.new_score, metric.delta)
                );
                println!("{}", paint(line, metric.delta));
            }
        }
        println!();
    }
}

fn format_score_change(old: Option<f64>, new: f64, delta: Option<f64>) -> String {
    match (old, delta) {
        (Some(old), Some(delta)) => {
            let sign = if delta > 0.0 { "+" } else { "" };
            format!("{:.1} -> {:.1} ({}{:.1})", old, new, sign, delta)
        }
        _ => format!("{:.1} (no base)", new),
    }
}

pub fn print_patch_delta_report(
    report: &PatchDeltaReport,
    format: OutputFormat,
    no_color: bool,
    saved_path: Option<&Path>,
) {
    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let colors_enabled = !no_color && io::stdout().is_terminal();
            let new_count = report.file_diffs.iter().filter(|f| f.is_new).count();
            println!(
                "Score changes vs {} ({} changed files, {} without a base version)",
                report.base,
                report.file_diffs.len(),
                new_count
            );
            println!();
            print_file_diffs(&report.file_diffs, colors_enabled);

            if let Some(max_drop) = report.max_regression {
                if report.regressions.is_empty() {
                    let line = format!("No regressions (allowed drop: {:.1}).", max_drop);
                    println!("{}", if colors_enabled { colorize(&line, "32") } else { line });
                } else {
                    let header = format!("Regressions (allowed drop: {:.1}):", max_drop);
                    println!("{}", if colors_enabled { colorize(&header, "31;1") } else { header });
                    for regression in &report.regressions {
                        let detail = match (regression.kind, regression.old_composite) {
                            (RegressionKind::Dropped, Some(old)) => format!(
                                "{:.1} -> {:.1}",
                                old, regression.new_composite
                            ),
                            _ => format!(
                                "{:.1}, below {:.1} with no base to compare",
                                regression.new_composite, regression.fail_below
                            ),
                        };
                        println!(
                            "  {} [{}]: {}",
                            regression.relative_path, regression.profile_name, detail
                        );
                    }
                }
            }

            let token_line = format!(
                "Tokens consumed (base + working tree): {} input · {} output · Est. cost: ${:.5}",
                report.usage.input_tokens, report.usage.output_tokens, report.usage.estimated_cost_usd
            );
            println!("{}", if colors_enabled { colorize(&token_line, "36") } else { token_line });

            if let Some(path) = saved_path {
                println!("✔ Working-tree report saved to {}", path.display());
            }
        }
    }
}

/// The likelier options of an enum answer, e.g. ` [good 45% · fair 40%]`, which explain a
/// normalized score that falls between options. Empty when one option holds nearly everything.
fn format_distribution(metric: &MetricEvaluation) -> String {
    if metric.metric_type != MetricType::Enum {
        return String::new();
    }
    let Some(probabilities) = &metric.probabilities else {
        return String::new();
    };
    let mut likely: Vec<(&String, f64)> = probabilities
        .iter()
        .map(|(option, p)| (option, *p))
        .filter(|(_, p)| *p >= 0.1)
        .collect();
    if likely.len() < 2 {
        return String::new();
    }
    likely.sort_by(|a, b| b.1.total_cmp(&a.1));
    let parts: Vec<String> = likely
        .iter()
        .map(|(option, p)| format!("{} {}%", option, (p * 100.0).round() as u64))
        .collect();
    format!(" [{}]", parts.join(" · "))
}

fn excluded_suffix(metric: &MetricEvaluation, min_confidence: f64) -> String {
    if metric.not_applicable {
        " [excluded: not applicable to this file]".to_string()
    } else if metric.excluded_low_confidence {
        format!(
            " [excluded: below {}% min conf.]",
            (min_confidence * 100.0).round() as u64
        )
    } else {
        String::new()
    }
}

fn colorize(s: &str, ansi_code: &str) -> String {
    format!("\x1b[{}m{}\x1b[0m", ansi_code, s)
}
