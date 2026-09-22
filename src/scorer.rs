use std::collections::HashMap;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    /// Confidence fell below the profile's `min_confidence`, so this metric is reported but
    /// does not count toward the composite score.
    #[serde(default)]
    pub excluded_low_confidence: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileEvaluation {
    pub profile_name: String,
    pub composite_score: f64,
    pub fail_below: f64,
    #[serde(default)]
    pub min_confidence: f64,
    pub passed: bool,
    /// Every metric was excluded for low confidence: `composite_score` is then computed over
    /// all metrics for reference only, and the profile passes without being judged.
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
    pub profiles: Vec<ProfileEvaluation>,
}

/// Bumped whenever the same raw answers would produce different composite scores, so runs
/// scored under different rules aren't silently compared. Version 0 (legacy, field absent)
/// under-scored every scale metric by one level; version 1 counted every metric regardless of
/// confidence.
pub const SCORING_VERSION: u32 = 2;

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

/// Evaluates all metrics for a file against the provided profiles and calculates composite scores.
pub fn evaluate_file_with_metrics(
    profiles: &[Profile],
    metric_results: &HashMap<String, CachedMetricResult>,
) -> Vec<ProfileEvaluation> {
    let mut profile_evals = Vec::with_capacity(profiles.len());

    for profile in profiles {
        let min_confidence = profile.effective_min_confidence();
        let mut confident = WeightedSum::default();
        let mut everything = WeightedSum::default();
        let mut metric_evals = Vec::with_capacity(profile.metrics.len());

        for metric in &profile.metrics {
            if let Some(cached) = metric_results.get(&metric.id) {
                let normalized = normalize_metric_score(metric, &cached.value);
                let weight = if metric.weight > 0.0 { metric.weight } else { 1.0 };
                let excluded_low_confidence = cached.confidence < min_confidence;

                everything.add(normalized, weight);
                if !excluded_low_confidence {
                    confident.add(normalized, weight);
                }

                metric_evals.push(MetricEvaluation {
                    metric_id: metric.id.clone(),
                    metric_type: metric.metric_type,
                    question: metric.question.clone(),
                    weight: metric.weight,
                    raw_value: cached.value.clone(),
                    confidence: cached.confidence,
                    normalized_score: normalized,
                    range: metric.range,
                    excluded_low_confidence,
                });
            }
        }

        let inconclusive = confident.weight == 0.0 && everything.weight > 0.0;
        let composite_score = if inconclusive {
            everything.mean()
        } else {
            confident.mean()
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
