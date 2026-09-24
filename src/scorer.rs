use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::cache::{CachedMetricResult, RawMetricValue};
use crate::profile::{Metric, MetricType, Profile};

pub use crate::pipeline::run_scan_pipeline;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricEvaluation {
    pub metric_id: String,
    pub metric_type: MetricType,
    pub question: String,
    pub weight: f64,
    pub raw_value: RawMetricValue,
    pub confidence: f64,
    pub normalized_score: f64,
    /// Jev's probability per possible answer (enum and binary metrics), which
    /// `normalized_score` averages over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<BTreeMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// The profile's rubric situation for `raw_value`, which says in words what the answer
    /// means. Absent for metrics without a rubric and in runs saved before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rubric: Option<String>,
    /// Jev's probability that the metric's `applies_when` condition holds for this file.
    /// Absent for metrics without a condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applicability: Option<f64>,
    /// Share of `weight` this metric carries in the composite, on [0, 1]: the product of its
    /// applicability share and confidence share (see `applicability_share`, `confidence_share`).
    /// Absent in runs scored before scoring version 4, where metrics counted fully or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inclusion: Option<f64>,
    /// Confidence fell below the profile's `min_confidence`, so the metric counts only partly,
    /// or not at all, and is not reported as a gap.
    #[serde(default)]
    pub excluded_low_confidence: bool,
    /// The metric's `applies_when` condition is more likely false than true for this file, so
    /// the metric counts only partly, or not at all, and is not reported as a gap.
    #[serde(default)]
    pub not_applicable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileEvaluation {
    pub profile_name: String,
    pub composite_score: f64,
    pub fail_below: f64,
    #[serde(default)]
    pub min_confidence: f64,
    pub passed: bool,
    /// Every metric was flagged low-confidence or not applicable: `composite_score` is then
    /// computed over all metrics at full weight for reference only, and the profile passes
    /// without being judged.
    #[serde(default)]
    pub inconclusive: bool,
    pub metrics: Vec<MetricEvaluation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RunUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEvaluation {
    pub path: PathBuf,
    pub relative_path: String,
    pub served_from_cache: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::cache::JevUsage>,
    /// Repository-relative paths of the related files shown to Jev with this file (`--context`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_files: Vec<String>,
    pub profiles: Vec<ProfileEvaluation>,
}

/// Bumped whenever the same raw answers would produce different composite scores, so runs
/// scored under different rules aren't silently compared. Version 0 (legacy, field absent)
/// under-scored every scale metric by one level; version 1 counted every metric regardless of
/// confidence; version 2 scored enum and binary answers by their top pick alone; version 3
/// counted a metric fully or not at all depending on whether its applicability and confidence
/// cleared a cutoff.
pub const SCORING_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanRunResult {
    #[serde(default)]
    pub scoring_version: u32,
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub target_path: String,
    pub total_files: usize,
    pub cached_files: usize,
    pub profiles_used: Vec<String>,
    pub files: Vec<FileEvaluation>,
    #[serde(default)]
    pub usage: RunUsage,
    pub passed: bool,
    pub exit_reason: Option<String>,
}

/// Normalizes any raw metric value (scale, binary, or enum) to the standard [1.0, 5.0] range.
pub fn normalize_metric_score(metric: &Metric, value: &RawMetricValue) -> f64 {
    match metric.metric_type {
        MetricType::Scale => normalize_scale_value(metric, value),
        MetricType::Binary => normalize_binary_value(metric, value),
        MetricType::Enum => normalize_enum_value(metric, value),
    }
}

fn normalize_scale_value(metric: &Metric, value: &RawMetricValue) -> f64 {
    if let RawMetricValue::Scale(v) = value {
        let range = metric.range.unwrap_or([1.0, 5.0]);
        let min = range[0];
        let max = range[1];
        if max <= min {
            return 5.0;
        }
        let norm = 1.0 + (v - min) / (max - min) * 4.0;
        norm.clamp(1.0, 5.0)
    } else {
        1.0
    }
}

fn normalize_binary_value(metric: &Metric, value: &RawMetricValue) -> f64 {
    if let RawMetricValue::Binary(b) = value {
        let good_value = metric.good_value.unwrap_or(false);
        if *b == good_value {
            5.0
        } else {
            1.0
        }
    } else {
        1.0
    }
}

fn normalize_enum_value(metric: &Metric, value: &RawMetricValue) -> f64 {
    let RawMetricValue::Enum(s) = value else {
        return 1.0;
    };
    enum_option_score(metric, s)
}

/// Score of one enum option on [1, 5]: from `score_map` when given, else by its position in
/// `options` (ascending, or descending for low/none/minimal … high/critical/severe scales).
fn enum_option_score(metric: &Metric, s: &str) -> f64 {
    if let Some(map) = &metric.score_map
        && let Some(score) = map.get(s) {
            return score.clamp(1.0, 5.0);
        }

    let Some(options) = &metric.options else {
        return 3.0;
    };

    let total = options.len();
    if total < 2 {
        return 5.0;
    }

    let opt_idx = options
        .iter()
        .position(|opt| opt.eq_ignore_ascii_case(s))
        .unwrap_or(0);

    let first = options[0].to_ascii_lowercase();
    let last = options[total - 1].to_ascii_lowercase();

    let lower_is_better = (first == "low" || first == "none" || first == "minimal")
        && (last == "critical" || last == "high" || last == "severe");

    let fraction = opt_idx as f64 / (total - 1) as f64;
    if lower_is_better {
        5.0 - fraction * 4.0
    } else {
        1.0 + fraction * 4.0
    }
}

/// Score of a metric answer on [1, 5]. Enum and binary answers that carry Jev's probabilities
/// score as the probability-weighted mean over every possible answer, as scale answers already
/// do, so a near-tie between two options lands between them instead of jumping with whichever
/// option narrowly wins. Answers without probabilities score by their chosen value.
pub fn score_metric_answer(metric: &Metric, answer: &CachedMetricResult) -> f64 {
    let expected = answer.probabilities.as_ref().and_then(|probabilities| match metric.metric_type {
        MetricType::Enum => expected_enum_score(metric, probabilities),
        MetricType::Binary => probabilities.get("true").map(|&p_true| {
            let p_good = if metric.good_value.unwrap_or(false) { p_true } else { 1.0 - p_true };
            1.0 + 4.0 * p_good.clamp(0.0, 1.0)
        }),
        MetricType::Scale => None,
    });
    expected.unwrap_or_else(|| normalize_metric_score(metric, &answer.value))
}

fn expected_enum_score(metric: &Metric, probabilities: &BTreeMap<String, f64>) -> Option<f64> {
    let options = metric.options.as_ref()?;
    let (mut weighted, mut total) = (0.0, 0.0);
    for option in options {
        if let Some((_, &p)) = probabilities.iter().find(|(key, _)| key.eq_ignore_ascii_case(option)) {
            weighted += p * enum_option_score(metric, option);
            total += p;
        }
    }
    (total > 0.0).then(|| weighted / total)
}

/// Evaluates all metrics for a file against the provided profiles and calculates composite scores.
pub fn evaluate_file_with_metrics(
    profiles: &[Profile],
    metric_results: &HashMap<String, CachedMetricResult>,
) -> Vec<ProfileEvaluation> {
    let mut profile_evals = Vec::with_capacity(profiles.len());

    for profile in profiles {
        let min_confidence = profile.effective_min_confidence();
        let mut included = WeightedSum::default();
        let mut everything = WeightedSum::default();
        let mut all_flagged = true;
        let mut metric_evals = Vec::with_capacity(profile.metrics.len());

        for metric in &profile.metrics {
            if let Some(cached) = metric_results.get(&metric.id) {
                let normalized = score_metric_answer(metric, cached);
                let weight = if metric.weight > 0.0 { metric.weight } else { 1.0 };
                let applicability = metric_results
                    .get(&metric.applicability_id())
                    .map(probability_of_true);
                let inclusion = applicability.map_or(1.0, applicability_share)
                    * confidence_share(cached.confidence, min_confidence);
                let excluded_low_confidence = cached.confidence < min_confidence;
                let not_applicable = applicability.is_some_and(|p| p < 0.5);
                all_flagged &= excluded_low_confidence || not_applicable;

                everything.add(normalized, weight);
                included.add(normalized, weight * inclusion);

                metric_evals.push(MetricEvaluation {
                    metric_id: metric.id.clone(),
                    metric_type: metric.metric_type,
                    question: metric.question.clone(),
                    weight: metric.weight,
                    raw_value: cached.value.clone(),
                    confidence: cached.confidence,
                    normalized_score: normalized,
                    probabilities: cached.probabilities.clone(),
                    range: metric.range,
                    matched_rubric: metric.rubric_for(&cached.value).map(str::to_string),
                    applicability,
                    inclusion: Some(inclusion),
                    excluded_low_confidence,
                    not_applicable,
                });
            }
        }

        let inconclusive = all_flagged && everything.weight > 0.0;
        let composite_score = if inconclusive {
            everything.mean()
        } else {
            included.mean()
        };
        let passed = inconclusive || composite_score >= profile.fail_below;

        profile_evals.push(ProfileEvaluation {
            profile_name: profile.name.clone(),
            composite_score,
            fail_below: profile.fail_below,
            min_confidence,
            passed,
            inconclusive,
            metrics: metric_evals,
        });
    }

    profile_evals
}

/// Probability that a yes/no answer is yes; answers cached without probabilities count as certain.
fn probability_of_true(answer: &CachedMetricResult) -> f64 {
    match (&answer.probabilities, &answer.value) {
        (Some(probabilities), _) if probabilities.contains_key("true") => {
            probabilities["true"].clamp(0.0, 1.0)
        }
        (_, RawMetricValue::Binary(true)) => 1.0,
        _ => 0.0,
    }
}

/// Share of its weight a metric carries given the probability that it applies: none below 0.25,
/// all above 0.75, and linearly more in between. Jev's answer for a borderline file drifts across
/// 0.5 between runs; counting a metric fully or not at all at 0.5 swung composites by more than
/// the regression allowance on identical code, while clearly (in)applicable metrics are unaffected.
pub fn applicability_share(p_applies: f64) -> f64 {
    ramp(p_applies, 0.25, 0.75)
}

/// Share of its weight a metric carries given its confidence: none below half the profile's
/// `min_confidence`, all from `min_confidence` up, and linearly more in between, so an answer
/// hovering around the threshold doesn't jump in and out of the composite.
pub fn confidence_share(confidence: f64, min_confidence: f64) -> f64 {
    if min_confidence <= 0.0 {
        return 1.0;
    }
    ramp(confidence, min_confidence / 2.0, min_confidence)
}

fn ramp(x: f64, low: f64, high: f64) -> f64 {
    ((x - low) / (high - low)).clamp(0.0, 1.0)
}

#[derive(Default)]
struct WeightedSum {
    total: f64,
    weight: f64,
}

impl WeightedSum {
    fn add(&mut self, score: f64, weight: f64) {
        self.total += score * weight;
        self.weight += weight;
    }

    /// Weighted mean rounded to one decimal; 5.0 when nothing was added.
    fn mean(&self) -> f64 {
        if self.weight > 0.0 {
            (self.total / self.weight * 10.0).round() / 10.0
        } else {
            5.0
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegressionKind {
    /// The composite dropped by more than the allowed amount versus the base.
    Dropped,
    /// No judged base to compare against (new file, or the base was inconclusive), and the new
    /// version scores below the profile's `fail_below`.
    BelowThresholdWithoutBase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Regression {
    pub relative_path: String,
    pub profile_name: String,
    pub kind: RegressionKind,
    pub old_composite: Option<f64>,
    pub new_composite: f64,
    pub fail_below: f64,
}

/// Regressions introduced by a change: judged profiles whose composite dropped by more than
/// `max_drop` from the base, and profiles with no judged base that fall below `fail_below`.
/// Scores that were already low at the base are deliberately not reported.
pub fn find_regressions(base: &ScanRunResult, head: &ScanRunResult, max_drop: f64) -> Vec<Regression> {
    let mut regressions = Vec::new();
    for file in &head.files {
        let base_file = base.files.iter().find(|f| f.relative_path == file.relative_path);
        for profile in file.profiles.iter().filter(|p| !p.inconclusive) {
            let base_profile = base_file
                .and_then(|f| f.profiles.iter().find(|p| p.profile_name == profile.profile_name))
                .filter(|p| !p.inconclusive);

            let kind = match base_profile {
                Some(base_profile) => {
                    let drop = round_to_tenth(base_profile.composite_score - profile.composite_score);
                    (drop > max_drop).then_some(RegressionKind::Dropped)
                }
                None => (!profile.passed).then_some(RegressionKind::BelowThresholdWithoutBase),
            };

            if let Some(kind) = kind {
                regressions.push(Regression {
                    relative_path: file.relative_path.clone(),
                    profile_name: profile.profile_name.clone(),
                    kind,
                    old_composite: base_profile.map(|p| p.composite_score),
                    new_composite: profile.composite_score,
                    fail_below: profile.fail_below,
                });
            }
        }
    }
    regressions
}

/// A metric of a changed file whose score dropped by more than the allowed amount versus the
/// base, even if the composite averaged the drop away.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricRegression {
    pub relative_path: String,
    pub profile_name: String,
    pub metric_id: String,
    pub old_score: f64,
    pub new_score: f64,
    /// `old_score - new_score` scaled by the smaller of the metric's `inclusion` in the two
    /// versions: a metric that barely counts in either version can't fail the gate on its own.
    pub weighted_drop: f64,
}

/// Metrics whose weighted drop from the base exceeds `max_drop`. Files without a base version
/// have nothing to compare; `find_regressions` covers them.
pub fn find_metric_regressions(base: &ScanRunResult, head: &ScanRunResult, max_drop: f64) -> Vec<MetricRegression> {
    let mut regressions = Vec::new();
    for file in &head.files {
        let Some(base_file) = base.files.iter().find(|f| f.relative_path == file.relative_path) else {
            continue;
        };
        for profile in &file.profiles {
            let Some(base_profile) = base_file.profiles.iter().find(|p| p.profile_name == profile.profile_name) else {
                continue;
            };
            for metric in &profile.metrics {
                let Some(base_metric) = base_profile.metrics.iter().find(|m| m.metric_id == metric.metric_id) else {
                    continue;
                };
                let counted = effective_inclusion(base_metric).min(effective_inclusion(metric));
                let weighted_drop = round_to_tenth((base_metric.normalized_score - metric.normalized_score) * counted);
                if weighted_drop > max_drop {
                    regressions.push(MetricRegression {
                        relative_path: file.relative_path.clone(),
                        profile_name: profile.profile_name.clone(),
                        metric_id: metric.metric_id.clone(),
                        old_score: round_to_tenth(base_metric.normalized_score),
                        new_score: round_to_tenth(metric.normalized_score),
                        weighted_drop,
                    });
                }
            }
        }
    }
    regressions
}

/// `inclusion`, or for runs scored before it was recorded, whether the metric counted at all.
fn effective_inclusion(metric: &MetricEvaluation) -> f64 {
    metric.inclusion.unwrap_or(if metric.excluded_low_confidence || metric.not_applicable { 0.0 } else { 1.0 })
}

/// Composites are reported to one decimal; compare differences at that precision too.
pub fn round_to_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}
