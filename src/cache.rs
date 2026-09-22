use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CacheError;
use crate::profile::{Metric, MetricType};
use crate::storage::{ensure_state_dir, get_state_dir};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RawMetricValue {
    Scale(f64),
    Enum(String),
    Binary(bool),
}

impl std::fmt::Display for RawMetricValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RawMetricValue::Scale(v) => {
                if v.fract() == 0.0 {
                    write!(f, "{}", *v as i64)
                } else {
                    write!(f, "{:.1}", v)
                }
            }
            RawMetricValue::Enum(s) => write!(f, "{}", s),
            RawMetricValue::Binary(b) => write!(f, "{}", b),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct JevUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedMetricResult {
    pub value: RawMetricValue,
    pub confidence: f64,
    /// Jev's probability for each possible answer: per option for enum metrics, `"true"` and
    /// `"false"` for binary ones. Scale answers carry none: Jev's score is already the
    /// probability-weighted position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<BTreeMap<String, f64>>,
}

/// The distribution of a binary answer given the probability that it is true.
pub fn binary_probabilities(p_true: f64) -> BTreeMap<String, f64> {
    let p_true = p_true.clamp(0.0, 1.0);
    BTreeMap::from([("true".to_string(), p_true), ("false".to_string(), 1.0 - p_true)])
}

/// Version 0 (legacy, field absent) stored scale answers as Jev's 0-based level position;
/// version 1 stores them as the profile's level label (position + range min).
/// Versions 0 and 1 stored binary confidence as max(p, 1 − p); version 2 stores |2p − 1|.
/// Versions before 3 kept only the chosen option of enum answers, not their probabilities.
pub const CACHE_FORMAT_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedFileResult {
    #[serde(default)]
    pub format_version: u32,
    pub file_hash: String,
    pub metrics_hash: String,
    pub timestamp: DateTime<Utc>,
    pub metrics: HashMap<String, CachedMetricResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<JevUsage>,
}

pub fn compute_cache_key(file_bytes: &[u8], metrics_hash: &str) -> (String, String) {
    let file_hash = blake3::hash(file_bytes).to_hex().to_string();

    let mut combined_hasher = blake3::Hasher::new();
    combined_hasher.update(file_hash.as_bytes());
    combined_hasher.update(b":");
    combined_hasher.update(metrics_hash.as_bytes());

    let cache_key = combined_hasher.finalize().to_hex().to_string();
    (cache_key, file_hash)
}

pub fn get_cache_dir(project_root: &Path) -> PathBuf {
    get_state_dir(project_root).join("cache")
}

/// Brings a cache entry written by an older format up to `CACHE_FORMAT_VERSION`. Returns `false`
/// when that isn't possible — enum answers cached without their probabilities — and the file
/// must be evaluated again, so every score in a run comes from the same scoring method.
pub fn upgrade_cached_result(result: &mut CachedFileResult, metrics: &[Metric]) -> bool {
    let version = result.format_version;
    for metric in metrics {
        let Some(cached) = result.metrics.get_mut(&metric.id) else {
            continue;
        };
        match (metric.metric_type, cached.value.clone()) {
            (MetricType::Scale, RawMetricValue::Scale(position)) if version < 1 => {
                cached.value = RawMetricValue::Scale(metric.scale_min_level() as f64 + position);
            }
            (MetricType::Binary, RawMetricValue::Binary(verdict)) if version < 3 => {
                if version < 2 {
                    cached.confidence = (2.0 * cached.confidence - 1.0).clamp(0.0, 1.0);
                }
                // Binary confidence is |2p − 1|, so the verdict and confidence give p exactly.
                let p_true = if verdict {
                    (1.0 + cached.confidence) / 2.0
                } else {
                    (1.0 - cached.confidence) / 2.0
                };
                cached.probabilities = Some(binary_probabilities(p_true));
            }
            (MetricType::Enum, RawMetricValue::Enum(_))
                if version < 3 && cached.probabilities.is_none() =>
            {
                return false;
            }
            _ => {}
        }
    }
    result.format_version = CACHE_FORMAT_VERSION;
    true
}

pub fn get_cached_result(project_root: &Path, key: &str) -> Option<CachedFileResult> {
    let cache_file = get_cache_dir(project_root).join(format!("{}.json", key));
    if !cache_file.exists() {
        return None;
    }

    let content = fs::read_to_string(&cache_file).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn put_cached_result(
    project_root: &Path,
    key: &str,
    result: &CachedFileResult,
) -> Result<PathBuf, CacheError> {
    let state_dir = ensure_state_dir(project_root)
        .map_err(|e| CacheError::WriteError(get_state_dir(project_root), e))?;
    let cache_dir = state_dir.join("cache");
    fs::create_dir_all(&cache_dir).map_err(|e| CacheError::WriteError(cache_dir.clone(), e))?;

    let cache_file = cache_dir.join(format!("{}.json", key));
    let json = serde_json::to_string_pretty(result)
        .map_err(|e| CacheError::ParseError(cache_file.clone(), e))?;

    fs::write(&cache_file, json).map_err(|e| CacheError::WriteError(cache_file.clone(), e))?;
    Ok(cache_file)
}
