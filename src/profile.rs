use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cache::RawMetricValue;
use crate::config::get_profiles_dir;
use crate::error::ProfileError;

pub const DEFAULT_QUALITY_JSON: &str = include_str!("../profiles/quality.json");
pub const DEFAULT_SECURITY_JSON: &str = include_str!("../profiles/security.json");
pub const DEFAULT_QA_JSON: &str = include_str!("../profiles/qa.json");

pub const BUILTIN_PROFILES: [(&str, &str); 3] = [
    ("quality", DEFAULT_QUALITY_JSON),
    ("security", DEFAULT_SECURITY_JSON),
    ("qa", DEFAULT_QA_JSON),
];

/// blake3 (over LF line endings) of every earlier release of the built-in profiles. `init`
/// copies built-ins into the profiles directory, where they take precedence; a copy matching one
/// of these was never edited by the user, so it must not shadow the current built-in.
const SUPERSEDED_BUILTIN_HASHES: &[&str] = &[
    // v0.1.0 quality, security, qa
    "4e97c283cdb1cbdf6a62a0a95f44195ca19f76e5a8ca35c51289f96dd9a02d89",
    "8d22a59f5d36a40642fb308522618e6089c1d50ee06dd5a0f0ab98ced2d6c07a",
    "e9d4658f72e4ac8c68ed73420d8722d44e830ac02e0839e152da40881fa6be07",
];

/// True when `content` is an unmodified copy of a current built-in profile (as `init` installs).
fn is_current_builtin(content: &str) -> bool {
    let content = content.replace("\r\n", "\n");
    BUILTIN_PROFILES
        .iter()
        .any(|(_, builtin)| builtin.replace("\r\n", "\n") == content)
}

/// True when `content` is an unmodified copy of an older built-in profile.
pub fn is_superseded_builtin(content: &str) -> bool {
    let hash = blake3::hash(content.replace("\r\n", "\n").as_bytes());
    SUPERSEDED_BUILTIN_HASHES.contains(&hash.to_hex().as_str())
}

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
    "min_confidence": { "type": "number", "minimum": 0, "maximum": 1 },
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
          },
          "rubric": {
            "oneOf": [
              { "type": "array", "items": { "type": "string", "minLength": 1 }, "minItems": 2 },
              { "type": "object", "additionalProperties": { "type": "string", "minLength": 1 } }
            ]
          },
          "applies_when": { "type": "string", "minLength": 1 }
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
    /// What each possible answer means, sent to Jev as the question's criteria.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rubric: Option<Rubric>,
    /// A condition asked as a separate yes/no question over the same file; when Jev judges it
    /// false, the metric is reported as not applicable and left out of the composite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_when: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Rubric {
    /// Scale metrics: one situation per level, lowest level first.
    Levels(Vec<String>),
    /// Enum metrics: one situation per option. Binary metrics: the `"true"` and `"false"` cases.
    Descriptions(BTreeMap<String, String>),
}

/// Jev's Score primitive accepts between 2 and 10 levels.
pub const MAX_SCALE_LEVELS: i64 = 10;

impl Metric {
    /// Lowest integer level label of a scale metric (the label Jev's position 0 maps to).
    pub fn scale_min_level(&self) -> i64 {
        self.range.map(|r| r[0] as i64).unwrap_or(1)
    }

    /// Highest integer level label of a scale metric.
    pub fn scale_max_level(&self) -> i64 {
        self.range.map(|r| r[1] as i64).unwrap_or(5)
    }

    /// Key under which the `applies_when` answer is requested from Jev and cached.
    pub fn applicability_id(&self) -> String {
        format!("{}:applies", self.id)
    }

    /// The rubric situation describing `value`: the nearest level for scale answers (which are
    /// probability-weighted positions between levels), the chosen option for enums, and the
    /// `"true"`/`"false"` case for binaries. `None` when the metric has no rubric.
    pub fn rubric_for(&self, value: &RawMetricValue) -> Option<&str> {
        match (self.rubric.as_ref()?, value) {
            (Rubric::Levels(levels), RawMetricValue::Scale(v)) => {
                let index = (v.round() as i64 - self.scale_min_level()).clamp(0, levels.len() as i64 - 1);
                levels.get(index as usize).map(String::as_str)
            }
            (Rubric::Descriptions(map), RawMetricValue::Enum(choice)) => map
                .iter()
                .find(|(option, _)| option.eq_ignore_ascii_case(choice))
                .map(|(_, description)| description.as_str()),
            (Rubric::Descriptions(map), RawMetricValue::Binary(b)) => {
                map.get(&b.to_string()).map(String::as_str)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub description: String,
    pub fail_below: f64,
    /// Metrics answered with confidence below this are excluded from the composite score.
    /// Scoring policy only: deliberately not part of the metrics hash, so changing it never
    /// invalidates cached answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
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

pub const DEFAULT_MIN_CONFIDENCE: f64 = 0.2;

impl Profile {
    pub fn effective_min_confidence(&self) -> f64 {
        self.min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE)
    }

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
            let invalid = |reason: String| ProfileError::InvalidMetric {
                profile: source_name.to_string(),
                metric_id: metric.id.clone(),
                reason,
            };

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
                    let level_count = metric.scale_max_level() - metric.scale_min_level() + 1;
                    if !(2..=MAX_SCALE_LEVELS).contains(&level_count) {
                        return Err(invalid(format!(
                            "scale range {:?} yields {} levels; Jev accepts 2 to {}",
                            range, level_count, MAX_SCALE_LEVELS
                        )));
                    }
                    match &metric.rubric {
                        None => {}
                        Some(Rubric::Levels(levels)) if levels.len() as i64 == level_count => {}
                        Some(_) => {
                            return Err(invalid(format!(
                                "scale rubric must be a list of exactly {} level descriptions, lowest first",
                                level_count
                            )));
                        }
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
                    let expected: BTreeSet<&str> = options.iter().map(String::as_str).collect();
                    check_description_keys(&metric.rubric, &expected, "one description per option")
                        .map_err(invalid)?;
                }
                MetricType::Binary => {
                    let expected = BTreeSet::from(["true", "false"]);
                    check_description_keys(&metric.rubric, &expected, "\"true\" and \"false\" descriptions")
                        .map_err(invalid)?;
                }
            }
        }

        Ok(())
    }

    pub fn metrics_hash(&self) -> String {
        compute_active_metrics_hash(std::slice::from_ref(self))
    }
}

fn check_description_keys(
    rubric: &Option<Rubric>,
    expected: &BTreeSet<&str>,
    what: &str,
) -> Result<(), String> {
    match rubric {
        None => Ok(()),
        Some(Rubric::Descriptions(map))
            if map.keys().map(String::as_str).collect::<BTreeSet<_>>() == *expected =>
        {
            Ok(())
        }
        Some(_) => Err(format!(
            "rubric must be an object with {}: {:?}",
            what, expected
        )),
    }
}

/// Hashes what is sent to Jev for each metric, so cached answers are reused exactly when the
/// questions are unchanged. Scoring policy (weight, good_value, score_map, thresholds) is left
/// out: changing it re-scores cached answers locally instead of re-querying.
pub fn compute_active_metrics_hash(profiles: &[Profile]) -> String {
    let mut canonical_entries: Vec<String> = Vec::new();

    for profile in profiles {
        for metric in &profile.metrics {
            let entry = serde_json::json!({
                "profile": profile.name,
                "metric_id": metric.id,
                "type": metric.metric_type,
                "question": metric.question,
                "range": metric.range,
                "options": metric.options,
                "rubric": metric.rubric,
                "applies_when": metric.applies_when,
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
            if !is_superseded_builtin(&content) {
                return Profile::validate_and_parse(&content, name);
            }
        }
    }

    let normalized_name = name.trim_end_matches(".json");
    match BUILTIN_PROFILES.iter().find(|(builtin, _)| *builtin == normalized_name) {
        Some((builtin, content)) => Profile::validate_and_parse(content, builtin),
        None => Err(ProfileError::NotFound(name.to_string())),
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

    for (name, content) in BUILTIN_PROFILES {
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
                    && let Ok(content) = fs::read_to_string(&path)
                    && !is_superseded_builtin(&content)
                    && !is_current_builtin(&content) {
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
