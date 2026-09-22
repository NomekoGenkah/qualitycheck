use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::get_profiles_dir;
use crate::error::ProfileError;

pub const DEFAULT_QUALITY_JSON: &str = include_str!("../profiles/quality.json");
pub const DEFAULT_SECURITY_JSON: &str = include_str!("../profiles/security.json");
pub const DEFAULT_QA_JSON: &str = include_str!("../profiles/qa.json");

pub const PROFILE_SCHEMA_JSON: &str = r#"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "QualityCheckProfile",
  "type": "object",
  "required": ["name", "description", "fail_below", "metrics"],
  "properties": {
    "$schema": { "type": "string" },
    "name": { "type": "string", "minLength": 1 },
    "description": { "type": "string" },
    "fail_below": { "type": "number", "minimum": 0 },
    "metrics": {
      "type": "array",
      "minItems": 1,
      "items": {
        "type": "object",
        "required": ["id", "type", "question", "weight"],
        "properties": {
          "id": { "type": "string", "minLength": 1 },
          "type": { "type": "string", "enum": ["scale", "enum", "binary"] },
          "question": { "type": "string", "minLength": 1 },
          "weight": { "type": "number", "minimum": 0 },
          "range": {
            "type": "array",
            "items": { "type": "number" },
            "minItems": 2,
            "maxItems": 2
          },
          "options": {
            "type": "array",
            "items": { "type": "string" },
            "minItems": 2
          },
          "good_value": { "type": "boolean" },
          "score_map": {
            "type": "object",
            "additionalProperties": { "type": "number" }
          }
        },
        "allOf": [
          {
            "if": { "properties": { "type": { "const": "scale" } } },
            "then": { "required": ["range"] }
          },
          {
            "if": { "properties": { "type": { "const": "enum" } } },
            "then": { "required": ["options"] }
          }
        ]
      }
    }
  }
}"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetricType {
    Scale,
    Enum,
    Binary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Metric {
    pub id: String,
    #[serde(rename = "type")]
    pub metric_type: MetricType,
    pub question: String,
    pub weight: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub good_value: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_map: Option<HashMap<String, f64>>,
}

impl Metric {
    /// Lowest integer level label of a scale metric (the label Jev's position 0 maps to).
    pub fn scale_min_level(&self) -> i64 {
        self.range.map(|r| r[0] as i64).unwrap_or(1)
    }

    /// Highest integer level label of a scale metric.
    pub fn scale_max_level(&self) -> i64 {
        self.range.map(|r| r[1] as i64).unwrap_or(5)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub description: String,
    pub fail_below: f64,
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub name: String,
    pub description: String,
    pub fail_below: f64,
    pub metric_count: usize,
    pub is_custom: bool,
    pub source_path: Option<PathBuf>,
}

impl Profile {
    pub fn validate_and_parse(raw_json: &str, source_name: &str) -> Result<Profile, ProfileError> {
        let value: Value = serde_json::from_str(raw_json)
            .map_err(|e| ProfileError::JsonError(source_name.to_string(), e))?;

        let schema_value: Value = serde_json::from_str(PROFILE_SCHEMA_JSON)
            .expect("Embedded PROFILE_SCHEMA_JSON must be valid JSON");

        let validator = jsonschema::validator_for(&schema_value)
            .map_err(|e| ProfileError::ValidationError(format!("Schema compilation error: {e}")))?;

        if let Err(err) = validator.validate(&value) {
            return Err(ProfileError::ValidationError(err.to_string()));
        }

        let profile: Profile = serde_json::from_value(value)
            .map_err(|e| ProfileError::JsonError(source_name.to_string(), e))?;

        profile.validate_logical(source_name)?;

        Ok(profile)
    }

    pub fn validate_logical(&self, source_name: &str) -> Result<(), ProfileError> {
        if self.metrics.is_empty() {
            return Err(ProfileError::EmptyMetrics(source_name.to_string()));
        }

        for metric in &self.metrics {
            if metric.weight < 0.0 {
                return Err(ProfileError::InvalidMetric {
                    profile: source_name.to_string(),
                    metric_id: metric.id.clone(),
                    reason: "weight cannot be negative".to_string(),
                });
            }

            match metric.metric_type {
                MetricType::Scale => {
                    let range = metric.range.ok_or_else(|| ProfileError::InvalidMetric {
                        profile: source_name.to_string(),
                        metric_id: metric.id.clone(),
                        reason: "scale metric must define range [min, max]".to_string(),
                    })?;
                    if range[0] >= range[1] {
                        return Err(ProfileError::InvalidMetric {
                            profile: source_name.to_string(),
                            metric_id: metric.id.clone(),
                            reason: format!(
                                "scale range min ({}) must be strictly less than max ({})",
                                range[0], range[1]
                            ),
                        });
                    }
                }
                MetricType::Enum => {
                    let options = metric.options.as_ref().ok_or_else(|| {
                        ProfileError::InvalidMetric {
                            profile: source_name.to_string(),
                            metric_id: metric.id.clone(),
                            reason: "enum metric must define options list".to_string(),
                        }
                    })?;
                    if options.len() < 2 {
                        return Err(ProfileError::InvalidMetric {
                            profile: source_name.to_string(),
                            metric_id: metric.id.clone(),
                            reason: "enum metric must contain at least 2 options".to_string(),
                        });
                    }
                }
                MetricType::Binary => {}
            }
        }

        Ok(())
    }

    pub fn metrics_hash(&self) -> String {
        compute_active_metrics_hash(std::slice::from_ref(self))
    }
}

pub fn compute_active_metrics_hash(profiles: &[Profile]) -> String {
    let mut canonical_entries: Vec<String> = Vec::new();

    for profile in profiles {
        for metric in &profile.metrics {
            let entry = serde_json::json!({
                "profile": profile.name,
                "metric_id": metric.id,
                "type": metric.metric_type,
                "question": metric.question,
                "weight": metric.weight,
                "range": metric.range,
                "options": metric.options,
                "good_value": metric.good_value,
                "score_map": metric.score_map,
            });
            canonical_entries.push(serde_json::to_string(&entry).unwrap());
        }
    }

    canonical_entries.sort();

    let mut hasher = blake3::Hasher::new();
    for entry in canonical_entries {
        hasher.update(entry.as_bytes());
        hasher.update(b"\n");
    }

    hasher.finalize().to_hex().to_string()
}

pub fn load_profile(name: &str) -> Result<Profile, ProfileError> {
    let direct_path = Path::new(name);
    if direct_path.exists() && direct_path.is_file() {
        let content = fs::read_to_string(direct_path)
            .map_err(|e| ProfileError::IoError(direct_path.to_path_buf(), e))?;
        return Profile::validate_and_parse(&content, name);
    }

    if let Ok(profiles_dir) = get_profiles_dir() {
        let candidate_with_ext = if name.ends_with(".json") {
            profiles_dir.join(name)
        } else {
            profiles_dir.join(format!("{}.json", name))
        };

        if candidate_with_ext.exists() {
            let content = fs::read_to_string(&candidate_with_ext)
                .map_err(|e| ProfileError::IoError(candidate_with_ext.clone(), e))?;
            return Profile::validate_and_parse(&content, name);
        }
    }

    let normalized_name = name.trim_end_matches(".json");
    match normalized_name {
        "quality" => Profile::validate_and_parse(DEFAULT_QUALITY_JSON, "quality"),
        "security" => Profile::validate_and_parse(DEFAULT_SECURITY_JSON, "security"),
        "qa" => Profile::validate_and_parse(DEFAULT_QA_JSON, "qa"),
        _ => Err(ProfileError::NotFound(name.to_string())),
    }
}

pub fn load_profiles_by_names(names: &[&str]) -> Result<Vec<Profile>, ProfileError> {
    let mut profiles = Vec::new();
    for name in names {
        let profile = load_profile(name)?;
        profiles.push(profile);
    }
    Ok(profiles)
}

pub fn list_available_profiles() -> Vec<ProfileSummary> {
    let mut summaries = HashMap::new();

    let defaults = [
        ("quality", DEFAULT_QUALITY_JSON),
        ("security", DEFAULT_SECURITY_JSON),
        ("qa", DEFAULT_QA_JSON),
    ];

    for (name, content) in defaults {
        if let Ok(profile) = Profile::validate_and_parse(content, name) {
            summaries.insert(
                name.to_string(),
                ProfileSummary {
                    name: profile.name,
                    description: profile.description,
                    fail_below: profile.fail_below,
                    metric_count: profile.metrics.len(),
                    is_custom: false,
                    source_path: None,
                },
            );
        }
    }

    if let Ok(profiles_dir) = get_profiles_dir()
        && let Ok(entries) = fs::read_dir(&profiles_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "json")
                    && let Ok(content) = fs::read_to_string(&path) {
                        let file_stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string();
                        if let Ok(profile) = Profile::validate_and_parse(&content, &file_stem) {
                            summaries.insert(
                                file_stem,
                                ProfileSummary {
                                    name: profile.name,
                                    description: profile.description,
                                    fail_below: profile.fail_below,
                                    metric_count: profile.metrics.len(),
                                    is_custom: true,
                                    source_path: Some(path),
                                },
                            );
                        }
                    }
            }
        }

    let mut result: Vec<ProfileSummary> = summaries.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}
