use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use serde_json::Value;

use crate::cache::{CachedMetricResult, JevUsage, RawMetricValue};
use crate::error::JevError;
use crate::profile::{Metric, MetricType};

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
    questions: HashMap<&'a str, JevQuestion<'a>>,
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

        let mut questions = HashMap::new();
        for metric in metrics {
            let (q_type, criteria) = match metric.metric_type {
                MetricType::Scale => {
                    let min_int = metric.range.map(|r| r[0] as i64).unwrap_or(1);
                    let max_int = metric.range.map(|r| r[1] as i64).unwrap_or(5);
                    let levels: Vec<String> = (min_int..=max_int).map(|i| i.to_string()).collect();
                    ("score", Some(serde_json::to_value(levels).unwrap_or(Value::Null)))
                }
                MetricType::Enum => {
                    let mut map = serde_json::Map::new();
                    if let Some(opts) = &metric.options {
                        for opt in opts {
                            map.insert(opt.clone(), Value::String(opt.clone()));
                        }
                    }
                    ("choice", Some(Value::Object(map)))
                }
                MetricType::Binary => ("noul", None),
            };

            questions.insert(
                metric.id.as_str(),
                JevQuestion {
                    question_type: q_type,
                    instructions: &metric.question,
                    criteria,
                },
            );
        }

        let request_body = JevRequest {
            model: "jev-latest",
            state: file_content,
            questions,
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
            let score_num = if let Some(n) = val.get("score").and_then(|v| v.as_f64()) {
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
                value: RawMetricValue::Scale(score_num),
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
        MetricType::Binary => {
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
                    "Missing or invalid boolean/noul for binary metric '{}': {:?}",
                    metric.id, val
                )));
            };

            // Jev calibrated probability: prob is probability statement is true
            let (verdict, confidence) = if prob >= 0.5 {
                (true, prob)
            } else {
                (false, 1.0 - prob)
            };

            Ok(CachedMetricResult {
                value: RawMetricValue::Binary(verdict),
                confidence: confidence.clamp(0.0, 1.0),
            })
        }
    }
}
