use std::collections::HashMap;
use tempfile::tempdir;

use qualitycheck::cache::{
    compute_cache_key, get_cached_result, put_cached_result, CachedFileResult, CachedMetricResult,
    RawMetricValue,
};
use qualitycheck::profile::{compute_active_metrics_hash, Metric, MetricType, Profile};

#[test]
fn test_cache_key_invalidation_on_metric_change() {
    let file_bytes = b"fn main() { println!(\"hello\"); }";

    let profile_a = Profile {
        schema: None,
        name: "quality".to_string(),
        description: "Desc".to_string(),
        fail_below: 3.0,
        metrics: vec![Metric {
            id: "naming_clarity".to_string(),
            metric_type: MetricType::Scale,
            question: "Question version 1".to_string(),
            weight: 1.0,
            range: Some([1.0, 5.0]),
            options: None,
            good_value: None,
            score_map: None,
        }],
    };

    let profile_b = Profile {
        schema: None,
        name: "quality".to_string(),
        description: "Desc".to_string(),
        fail_below: 3.0,
        metrics: vec![Metric {
            id: "naming_clarity".to_string(),
            metric_type: MetricType::Scale,
            question: "Question version 2 with updated wording".to_string(),
            weight: 1.0,
            range: Some([1.0, 5.0]),
            options: None,
            good_value: None,
            score_map: None,
        }],
    };

    let hash_a = compute_active_metrics_hash(&[profile_a]);
    let hash_b = compute_active_metrics_hash(&[profile_b]);

    assert_ne!(hash_a, hash_b, "Different questions must have different metric hashes");

    let (key_a, _) = compute_cache_key(file_bytes, &hash_a);
    let (key_b, _) = compute_cache_key(file_bytes, &hash_b);

    assert_ne!(key_a, key_b, "Changing metric question must invalidate cache key");
}

#[test]
fn test_cache_put_and_get() {
    let tmp = tempdir().unwrap();
    let project_root = tmp.path();

    let key = "abc123def456";
    assert!(get_cached_result(project_root, key).is_none());

    let mut metrics = HashMap::new();
    metrics.insert(
        "naming_clarity".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Scale(4.0),
            confidence: 0.95,
        },
    );
    metrics.insert(
        "has_dead_code".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Binary(false),
            confidence: 0.98,
        },
    );

    let result = CachedFileResult {
        file_hash: "f_hash_123".to_string(),
        metrics_hash: "m_hash_456".to_string(),
        timestamp: chrono::Utc::now(),
        metrics: metrics.clone(),
        usage: None,
    };

    let put_path = put_cached_result(project_root, key, &result).unwrap();
    assert!(put_path.exists());

    let loaded = get_cached_result(project_root, key).expect("Should load cached result");
    assert_eq!(loaded.file_hash, "f_hash_123");
    assert_eq!(loaded.metrics_hash, "m_hash_456");
    assert_eq!(loaded.metrics.len(), 2);
    assert_eq!(loaded.metrics["naming_clarity"].value, RawMetricValue::Scale(4.0));
    assert_eq!(loaded.metrics["has_dead_code"].value, RawMetricValue::Binary(false));
}
