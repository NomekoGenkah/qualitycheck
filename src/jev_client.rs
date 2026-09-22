use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use serde_json::Value;

use crate::cache::{CachedMetricResult, JevUsage, RawMetricValue};
use crate::error::JevError;
use crate::profile::{Metric, MetricType, Rubric};

#[derive(Debug, Clone)]
pub struct JevClient {
    client: reqwest::Client,
    api_key: String,
    endpoint_url: String,
}

#[derive(Debug, Serialize)]
struct JevRequest<'a> {
    model: &'a str,
    state: &'a str,
    questions: HashMap<String, JevQuestion<'a>>,
}

#[derive(Debug, Serialize)]
struct JevQuestion<'a> {
    #[serde(rename = "type")]
    question_type: &'a str,
    instructions: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    criteria: Option<Value>,
}

impl JevClient {
    pub fn new(api_key: String, endpoint_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            api_key,
            endpoint_url,
        }
    }

    pub async fn evaluate_file(
        &self,
        file_content: &str,
        metrics: &[Metric],
    ) -> Result<JevEvaluationResult, JevError> {
        if metrics.is_empty() {
            return Ok(JevEvaluationResult {
                metrics: HashMap::new(),
                usage: JevUsage::default(),
            });
        }

        let request_body = JevRequest {
            model: "jev-latest",
            state: file_content,
            questions: build_questions(metrics),
        };

        let mut headers = HeaderMap::new();
        let auth_val = format!("Bearer {}", self.api_key.trim());
        let mut header_value = HeaderValue::from_str(&auth_val)
            .map_err(|_| JevError::InvalidResponse("Invalid characters in API key header".to_string()))?;
        header_value.set_sensitive(true);
        headers.insert(AUTHORIZATION, header_value);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .client
            .post(&self.endpoint_url)
            .headers(headers)
            .json(&request_body)
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(JevError::Unauthorized);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(JevError::RateLimited);
        }
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(JevError::ApiStatus {
                status: status.as_u16(),
                message: error_text,
            });
        }

        let response_json: Value = response.json().await.map_err(|e| {
            JevError::InvalidResponse(format!("Failed to parse response JSON: {e}"))
        })?;

        let (input_tokens, output_tokens) = if let Some(usage) = response_json.get("usage") {
            let in_tok = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            let out_tok = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
            (in_tok, out_tok)
        } else {
            (0, 0)
        };

        let mut results = HashMap::new();
        for metric in metrics {
            let answer_val = extract_metric_answer(&response_json, &metric.id)
                .ok_or_else(|| JevError::MissingMetricAnswer(metric.id.clone()))?;

            let metric_res = parse_answer(answer_val, metric)?;
            results.insert(metric.id.clone(), metric_res);

            if metric.applies_when.is_some() {
                let applicability_id = metric.applicability_id();
                let answer_val = extract_metric_answer(&response_json, &applicability_id)
                    .ok_or_else(|| JevError::MissingMetricAnswer(applicability_id.clone()))?;
                results.insert(applicability_id.clone(), parse_binary(answer_val, &applicability_id)?);
            }
        }

        Ok(JevEvaluationResult {
            metrics: results,
            usage: JevUsage {
                input_tokens,
                output_tokens,
            },
        })
    }
}

/// One question per metric, plus a yes/no question for each metric's `applies_when`. They all
/// share the file as state and are answered independently in the same request.
fn build_questions(metrics: &[Metric]) -> HashMap<String, JevQuestion<'_>> {
    let mut questions = HashMap::new();
    for metric in metrics {
        let (question_type, criteria) = match metric.metric_type {
            MetricType::Scale => {
                let levels: Vec<String> = match &metric.rubric {
                    Some(Rubric::Levels(levels)) => levels.clone(),
                    _ => (metric.scale_min_level()..=metric.scale_max_level())
                        .map(|i| i.to_string())
                        .collect(),
                };
                ("score", Some(serde_json::json!(levels)))
            }
            MetricType::Enum => {
                let described = |opt: &String| match &metric.rubric {
                    Some(Rubric::Descriptions(map)) => map.get(opt).cloned().unwrap_or_else(|| opt.clone()),
                    _ => opt.clone(),
                };
                let map: serde_json::Map<String, Value> = metric
                    .options
                    .iter()
                    .flatten()
                    .map(|opt| (opt.clone(), Value::String(described(opt))))
                    .collect();
                ("choice", Some(Value::Object(map)))
            }
            MetricType::Binary => {
                let criteria = match &metric.rubric {
                    Some(Rubric::Descriptions(map)) => Some(serde_json::json!(map)),
                    _ => None,
                };
                ("noul", criteria)
            }
        };

        questions.insert(
            metric.id.clone(),
            JevQuestion {
                question_type,
                instructions: &metric.question,
                criteria,
            },
        );

        if let Some(condition) = &metric.applies_when {
            questions.insert(
                metric.applicability_id(),
                JevQuestion {
                    question_type: "noul",
                    instructions: condition,
                    criteria: None,
                },
            );
        }
    }
    questions
}

#[derive(Debug, Clone)]
pub struct JevEvaluationResult {
    pub metrics: HashMap<String, CachedMetricResult>,
    pub usage: JevUsage,
}

fn extract_metric_answer<'a>(root: &'a Value, metric_id: &str) -> Option<&'a Value> {
    if let Some(answers) = root.get("answers").and_then(|a| a.as_object())
        && let Some(ans) = answers.get(metric_id) {
            return Some(ans);
        }

    for group in ["scores", "choices", "nouls"] {
        if let Some(group_obj) = root.get(group).and_then(|g| g.as_object())
            && let Some(ans) = group_obj.get(metric_id) {
                return Some(ans);
            }
    }

    if let Some(ans) = root.get(metric_id) {
        return Some(ans);
    }

    None
}

fn parse_answer(val: &Value, metric: &Metric) -> Result<CachedMetricResult, JevError> {
    match metric.metric_type {
        MetricType::Scale => {
            // Jev returns a 0-based position on the criteria levels (0..=N-1), not the level
            // label; shift it onto the profile's own range so the rest of the code sees labels.
            let position = if let Some(n) = val.get("score").and_then(|v| v.as_f64()) {
                n
            } else if let Some(n) = val.get("value").and_then(|v| v.as_f64()) {
                n
            } else if let Some(n) = val.as_f64() {
                n
            } else if let Some(s) = val.as_str().and_then(|s| s.parse::<f64>().ok()) {
                s
            } else {
                return Err(JevError::InvalidResponse(format!(
                    "Missing or invalid score for scale metric '{}': {:?}",
                    metric.id, val
                )));
            };

            let confidence = val
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);

            Ok(CachedMetricResult {
                value: RawMetricValue::Scale(metric.scale_min_level() as f64 + position),
                confidence: confidence.clamp(0.0, 1.0),
            })
        }
        MetricType::Enum => {
            let choice_str = if let Some(s) = val.get("choice").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(s) = val.get("value").and_then(|v| v.as_str()) {
                s.to_string()
            } else if let Some(s) = val.as_str() {
                s.to_string()
            } else {
                return Err(JevError::InvalidResponse(format!(
                    "Missing or invalid choice for enum metric '{}': {:?}",
                    metric.id, val
                )));
            };

            let confidence = val
                .get("confidence")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);

            Ok(CachedMetricResult {
                value: RawMetricValue::Enum(choice_str),
                confidence: confidence.clamp(0.0, 1.0),
            })
        }
        MetricType::Binary => parse_binary(val, &metric.id),
    }
}

fn parse_binary(val: &Value, id: &str) -> Result<CachedMetricResult, JevError> {
    // Noul answers can be:
    // 1. { "noul": 0.05 } (calibrated probability of TRUE)
    // 2. { "value": false, "confidence": 0.95 }
    // 3. raw boolean: false
    // 4. raw float: 0.05
    if let Some(b) = val.get("value").and_then(|v| v.as_bool()) {
        let conf = val.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0);
        return Ok(CachedMetricResult {
            value: RawMetricValue::Binary(b),
            confidence: conf.clamp(0.0, 1.0),
        });
    }

    if let Some(b) = val.as_bool() {
        return Ok(CachedMetricResult {
            value: RawMetricValue::Binary(b),
            confidence: 1.0,
        });
    }

    let prob = if let Some(p) = val.get("noul").and_then(|v| v.as_f64()) {
        p
    } else if let Some(p) = val.as_f64() {
        p
    } else {
        return Err(JevError::InvalidResponse(format!(
            "Missing or invalid boolean/noul for binary question '{}': {:?}",
            id, val
        )));
    };

    // Jev calibrated probability: prob is probability statement is true. Nouls carry no
    // confidence of their own; use Jev's Choice formula for two options, (2·p_max − 1),
    // so a coin-flip answer scores 0 like a flat Choice/Score distribution does.
    Ok(CachedMetricResult {
        value: RawMetricValue::Binary(prob >= 0.5),
        confidence: (2.0 * prob - 1.0).abs().clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::load_profile;

    #[test]
    fn rubrics_become_criteria_and_conditions_become_nouls() {
        let profile = load_profile("quality").unwrap();
        let questions = build_questions(&profile.metrics);
        let request = serde_json::to_value(&questions).unwrap();

        let naming = &request["naming_clarity"];
        assert_eq!(naming["type"], "score");
        assert_eq!(naming["criteria"].as_array().unwrap().len(), 5);
        assert!(naming["criteria"][0].as_str().unwrap().starts_with("Most names are cryptic"));

        assert_eq!(request["complexity_level"]["type"], "choice");
        assert!(request["complexity_level"]["criteria"]["low"]
            .as_str()
            .unwrap()
            .starts_with("Functions are short"));

        assert_eq!(request["has_dead_code"]["type"], "noul");
        assert!(request["has_dead_code"]["criteria"]["true"].is_string());

        let applies = &request["naming_clarity:applies"];
        assert_eq!(applies["type"], "noul");
        assert!(applies["instructions"].as_str().unwrap().starts_with("The file declares"));
        assert!(request.get("has_dead_code:applies").is_none());
    }
}
