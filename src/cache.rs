use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::CacheError;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedFileResult {
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
    project_root.join(".qualitycheck").join("cache")
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
    let cache_dir = get_cache_dir(project_root);
    fs::create_dir_all(&cache_dir).map_err(|e| CacheError::WriteError(cache_dir.clone(), e))?;

    let cache_file = cache_dir.join(format!("{}.json", key));
    let json = serde_json::to_string_pretty(result)
        .map_err(|e| CacheError::ParseError(cache_file.clone(), e))?;

    fs::write(&cache_file, json).map_err(|e| CacheError::WriteError(cache_file.clone(), e))?;
    Ok(cache_file)
}
