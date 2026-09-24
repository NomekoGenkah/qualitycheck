//! Output for CI: GitHub Actions annotations (`--format github`) and a Markdown summary
//! (`--format markdown`, and the step summary under `--format github`).

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::output::PatchDeltaReport;
use crate::scorer::{FileEvaluation, MetricEvaluation, ProfileEvaluation, RegressionKind, ScanRunResult};

/// GitHub shows at most this many annotations of each level per step.
const MAX_ANNOTATIONS_PER_LEVEL: usize = 10;
/// Rows per Markdown table, so a large scan keeps the summary readable.
const MAX_SUMMARY_ROWS: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// Part of why the gate failed: a regression, or with no base to compare, a failing profile.
    Error,
    Warning,
}

/// One metric that scores below its profile's threshold or regressed, or one profile that
/// regressed as a whole.
#[derive(Debug, Clone)]
struct Finding {
    level: Level,
    /// Repository-relative, as GitHub expects.
    path: String,
    /// Where `--explain` located it; the whole file otherwise.
    lines: Option<(usize, usize)>,
    title: String,
    message: String,
    /// How far below its threshold (or its base) the score is; orders findings of the same
    /// level, largest first, so profiles with different thresholds compare fairly.
    shortfall: f64,
}

/// `--format github`: an annotation per finding on stdout and, when run in GitHub Actions, the
/// Markdown summary appended to the step summary.
pub fn print_github(run: &ScanRunResult, delta: Option<&PatchDeltaReport>) {
    let findings = collect_findings(run, delta);
    let mut omitted = 0;
    for level in [Level::Error, Level::Warning] {
        let of_level: Vec<&Finding> = findings.iter().filter(|f| f.level == level).collect();
        omitted += of_level.len().saturating_sub(MAX_ANNOTATIONS_PER_LEVEL);
        for finding in of_level.into_iter().take(MAX_ANNOTATIONS_PER_LEVEL) {
            println!("{}", annotation(finding));
        }
    }
    if omitted > 0 {
        println!("::notice title=qualitycheck::{omitted} more finding(s) are listed in the step summary");
    }

    if let Some(summary_path) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let summary = render_markdown(run, delta, &findings);
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&summary_path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, summary.as_bytes()));
        if let Err(err) = written {
            eprintln!("Could not write the step summary: {err}");
        }
    }
}

/// `--format markdown`: the summary on stdout, e.g. for a pull request comment.
pub fn print_markdown(run: &ScanRunResult, delta: Option<&PatchDeltaReport>) {
    print!("{}", render_markdown(run, delta, &collect_findings(run, delta)));
}

fn collect_findings(run: &ScanRunResult, delta: Option<&PatchDeltaReport>) -> Vec<Finding> {
    let regressed_profiles: HashSet<(&str, &str)> = delta
        .map(|d| d.regressions.iter().map(|r| (r.relative_path.as_str(), r.profile_name.as_str())).collect())
        .unwrap_or_default();
    let regressed_metrics: HashSet<(&str, &str, &str)> = delta
        .map(|d| {
            d.metric_regressions
                .iter()
                .map(|r| (r.relative_path.as_str(), r.profile_name.as_str(), r.metric_id.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let mut findings = Vec::new();
    for file in &run.files {
        let path = repository_path(file);
        for profile in &file.profiles {
            let profile_regressed = regressed_profiles.contains(&(file.relative_path.as_str(), profile.profile_name.as_str()));
            for metric in &profile.metrics {
                let metric_regressed = regressed_metrics.contains(&(
                    file.relative_path.as_str(),
                    profile.profile_name.as_str(),
                    metric.metric_id.as_str(),
                ));
                let below = metric.counts() && metric.normalized_score < profile.fail_below;
                if !below && !metric_regressed {
                    continue;
                }
                let failing = match delta {
                    Some(_) => profile_regressed || metric_regressed,
                    None => !profile.passed && !profile.inconclusive,
                };
                findings.push(metric_finding(&path, profile, metric, failing));
            }
        }
    }

    if let Some(delta) = delta {
        for regression in &delta.regressions {
            let Some(file) = run.files.iter().find(|f| f.relative_path == regression.relative_path) else {
                continue;
            };
            let message = match (regression.kind, regression.old_composite) {
                (RegressionKind::Dropped, Some(old)) => format!(
                    "The {} composite dropped from {:.1} to {:.1}, more than the allowed {:.1}.",
                    regression.profile_name,
                    old,
                    regression.new_composite,
                    delta.max_regression.unwrap_or_default()
                ),
                _ => format!(
                    "The {} composite is {:.1}, below {:.1}, with no base version to compare against.",
                    regression.profile_name, regression.new_composite, regression.fail_below
                ),
            };
            findings.push(Finding {
                level: Level::Error,
                path: repository_path(file),
                lines: None,
                title: format!("qualitycheck: {} regressed", regression.profile_name),
                message,
                shortfall: regression.old_composite.unwrap_or(regression.fail_below) - regression.new_composite,
            });
        }
    }

    findings.sort_by(|a, b| a.level.cmp(&b.level).then(b.shortfall.total_cmp(&a.shortfall)));
    findings
}

fn metric_finding(path: &str, profile: &ProfileEvaluation, metric: &MetricEvaluation, failing: bool) -> Finding {
    let hotspots = metric.evidence.as_ref().map(|e| e.hotspots.as_slice()).unwrap_or_default();
    let mut message = metric
        .matched_rubric
        .clone()
        .unwrap_or_else(|| format!("{} answered {}.", metric.metric_id, metric.raw_value));
    if hotspots.len() > 1 {
        let others: Vec<String> = hotspots[1..].iter().map(|r| format!("L{}-{}", r.start_line, r.end_line)).collect();
        let _ = write!(message, " Also at {}.", others.join(", "));
    } else if metric.evidence.as_ref().is_some_and(|e| e.hotspots.is_empty()) {
        message.push_str(" No single region stands out: this concerns the file as a whole.");
    }
    Finding {
        level: if failing { Level::Error } else { Level::Warning },
        path: path.to_string(),
        lines: hotspots.first().map(|r| (r.start_line, r.end_line)),
        title: format!(
            "qualitycheck: {} {:.1}/5 ({})",
            metric.metric_id, metric.normalized_score, profile.profile_name
        ),
        message,
        shortfall: profile.fail_below - metric.normalized_score,
    }
}

/// The file's path relative to the workspace GitHub checked out (or the current directory),
/// since display paths are relative to the scanned target.
fn repository_path(file: &FileEvaluation) -> String {
    let root = std::env::var_os("GITHUB_WORKSPACE")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let absolute = if file.path.is_absolute() {
        file.path.clone()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(&file.path)).unwrap_or_else(|_| file.path.clone())
    };
    let relative = root
        .as_deref()
        .and_then(|root| absolute.strip_prefix(root).ok())
        .unwrap_or(&file.path);
    let display = relative.to_string_lossy().replace('\\', "/");
    display.strip_prefix("./").map(str::to_string).unwrap_or(display)
}

fn annotation(finding: &Finding) -> String {
    let level = match finding.level {
        Level::Error => "error",
        Level::Warning => "warning",
    };
    let mut properties = format!("file={}", escape_property(&finding.path));
    if let Some((start, end)) = finding.lines {
        let _ = write!(properties, ",line={start},endLine={end}");
    }
    let _ = write!(properties, ",title={}", escape_property(&finding.title));
    format!("::{level} {properties}::{}", escape_data(&finding.message))
}

/// Workflow command escaping: https://github.com/actions/toolkit/blob/main/packages/core/src/command.ts
fn escape_data(value: &str) -> String {
    value.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

fn escape_property(value: &str) -> String {
    escape_data(value).replace(':', "%3A").replace(',', "%2C")
}

fn render_markdown(run: &ScanRunResult, delta: Option<&PatchDeltaReport>, findings: &[Finding]) -> String {
    let mut md = String::from("## qualitycheck\n\n");
    let files = run.files.len();
    let headline = match delta {
        Some(delta) => {
            let gated = delta.max_regression.is_some() || delta.max_metric_regression.is_some();
            let regressions = delta.regressions.len() + delta.metric_regressions.len();
            match (gated, regressions) {
                (true, 0) => format!("✅ No regressions vs `{}`", delta.base),
                (true, n) => format!("❌ {n} regression(s) vs `{}`", delta.base),
                (false, _) => format!("Score changes vs `{}`", delta.base),
            }
        }
        None if run.passed => "✅ Every file meets its profile thresholds".to_string(),
        None => {
            let failing = run.files.iter().filter(|f| f.profiles.iter().any(|p| !p.passed)).count();
            format!("⚠️ {failing} file(s) below a profile threshold")
        }
    };
    let _ = writeln!(md, "{headline} · {files} file(s) · {} finding(s)\n", findings.len());

    // Composite per file and profile, worst files first.
    let profiles = &run.profiles_used;
    let _ = writeln!(md, "| File | {} |", profiles.join(" | "));
    let _ = writeln!(md, "|---|{}", "---|".repeat(profiles.len()));
    let mut ordered: Vec<&FileEvaluation> = run.files.iter().collect();
    let worst = |f: &FileEvaluation| f.profiles.iter().map(|p| p.composite_score).fold(f64::INFINITY, f64::min);
    ordered.sort_by(|a, b| worst(a).total_cmp(&worst(b)));
    for file in ordered.iter().take(MAX_SUMMARY_ROWS) {
        let cells: Vec<String> = profiles
            .iter()
            .map(|name| match file.profiles.iter().find(|p| &p.profile_name == name) {
                Some(profile) => composite_cell(file, profile, delta),
                None => "–".to_string(),
            })
            .collect();
        let _ = writeln!(md, "| `{}` | {} |", repository_path(file), cells.join(" | "));
    }
    if ordered.len() > MAX_SUMMARY_ROWS {
        let _ = writeln!(md, "\n…and {} more file(s).", ordered.len() - MAX_SUMMARY_ROWS);
    }

    if !findings.is_empty() {
        md.push_str("\n### Findings\n\n| | File | Finding | Where | What was found |\n|---|---|---|---|---|\n");
        for finding in findings.iter().take(MAX_SUMMARY_ROWS) {
            let icon = match finding.level {
                Level::Error => "❌",
                Level::Warning => "⚠️",
            };
            let lines = finding.lines.map(|(s, e)| format!("L{s}-{e}")).unwrap_or_else(|| "–".to_string());
            let title = finding.title.trim_start_matches("qualitycheck: ");
            let _ = writeln!(
                md,
                "| {icon} | `{}` | {} | {lines} | {} |",
                finding.path,
                escape_cell(title),
                escape_cell(&finding.message)
            );
        }
        if findings.len() > MAX_SUMMARY_ROWS {
            let _ = writeln!(md, "\n…and {} more finding(s).", findings.len() - MAX_SUMMARY_ROWS);
        }
    }

    let usage = delta.map(|d| &d.usage).unwrap_or(&run.usage);
    let _ = writeln!(
        md,
        "\n<sub>Jev: {} input · {} output tokens · ${:.5}</sub>\n",
        usage.input_tokens, usage.output_tokens, usage.estimated_cost_usd
    );
    md
}

/// `4.1`, or with a base `4.4 → 4.1 (-0.3)`; bold with ✗ when the profile fails its threshold.
fn composite_cell(file: &FileEvaluation, profile: &ProfileEvaluation, delta: Option<&PatchDeltaReport>) -> String {
    if profile.inconclusive {
        return "inconclusive".to_string();
    }
    let change = delta
        .and_then(|d| d.file_diffs.iter().find(|f| f.relative_path == file.relative_path))
        .and_then(|f| f.profile_diffs.iter().find(|p| p.profile_name == profile.profile_name))
        .and_then(|p| Some((p.old_composite?, p.delta?)));
    let score = match change {
        Some((old, change)) => format!("{old:.1} → {:.1} ({change:+.1})", profile.composite_score),
        None => format!("{:.1}", profile.composite_score),
    };
    if profile.passed { score } else { format!("**{score}** ✗") }
}

fn escape_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_command_values_are_escaped() {
        assert_eq!(escape_data("50% done\nnext"), "50%25 done%0Anext");
        assert_eq!(escape_property("a:b,c"), "a%3Ab%2Cc");
        assert_eq!(escape_cell("a | b\nc"), "a \\| b c");
    }

    #[test]
    fn annotations_carry_the_hotspot_lines() {
        let finding = Finding {
            level: Level::Error,
            path: "src/a.ts".to_string(),
            lines: Some((125, 161)),
            title: "qualitycheck: injection_risk 2.9/5 (security)".to_string(),
            message: "Strings are built from input.".to_string(),
            shortfall: 0.6,
        };
        assert_eq!(
            annotation(&finding),
            "::error file=src/a.ts,line=125,endLine=161,title=qualitycheck%3A injection_risk 2.9/5 (security)::Strings are built from input."
        );
    }
}
