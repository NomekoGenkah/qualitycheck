use std::collections::HashMap;
use tempfile::tempdir;

use qualitycheck::cache::{
    compute_cache_key, get_cached_result, put_cached_result, upgrade_cached_result,
    CachedFileResult, CachedMetricResult, RawMetricValue, CACHE_FORMAT_VERSION,
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
        min_confidence: None,
        metrics: vec![Metric {
            id: "naming_clarity".to_string(),
            metric_type: MetricType::Scale,
            question: "Question version 1".to_string(),
            weight: 1.0,
            range: Some([1.0, 5.0]),
            options: None,
            good_value: None,
            score_map: None,
            rubric: None,
            applies_when: None,
        }],
    };

    let profile_b = Profile {
        schema: None,
        name: "quality".to_string(),
        description: "Desc".to_string(),
        fail_below: 3.0,
        min_confidence: None,
        metrics: vec![Metric {
            id: "naming_clarity".to_string(),
            metric_type: MetricType::Scale,
            question: "Question version 2 with updated wording".to_string(),
            weight: 1.0,
            range: Some([1.0, 5.0]),
            options: None,
            good_value: None,
            score_map: None,
            rubric: None,
            applies_when: None,
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
        format_version: CACHE_FORMAT_VERSION,
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

fn scale_metric(id: &str, range: [f64; 2]) -> Metric {
    Metric {
        id: id.to_string(),
        metric_type: MetricType::Scale,
        question: "Q".to_string(),
        weight: 1.0,
        range: Some(range),
        options: None,
        good_value: None,
        score_map: None,
        rubric: None,
        applies_when: None,
    }
}

fn binary_metric(id: &str) -> Metric {
    Metric {
        id: id.to_string(),
        metric_type: MetricType::Binary,
        question: "Q".to_string(),
        weight: 1.0,
        range: None,
        options: None,
        good_value: Some(false),
        score_map: None,
        rubric: None,
        applies_when: None,
    }
}

#[test]
fn test_legacy_cache_entry_scale_positions_are_upgraded() {
    // Entries written before format_version existed stored Jev's 0-based level position.
    let legacy_json = r#"{
        "file_hash": "f",
        "metrics_hash": "m",
        "timestamp": "2026-09-21T17:31:26Z",
        "metrics": {
            "naming_clarity": { "value": 3.5, "confidence": 0.65 },
            "zero_based": { "value": 2.0, "confidence": 0.9 },
            "complexity_level": { "value": "medium", "confidence": 0.4 },
            "has_dead_code": { "value": false, "confidence": 0.95 }
        }
    }"#;
    let mut entry: CachedFileResult = serde_json::from_str(legacy_json).unwrap();
    assert_eq!(entry.format_version, 0);

    let metrics = vec![
        scale_metric("naming_clarity", [1.0, 5.0]),
        scale_metric("zero_based", [0.0, 4.0]),
        binary_metric("has_dead_code"),
    ];
    upgrade_cached_result(&mut entry, &metrics);

    assert_eq!(entry.format_version, CACHE_FORMAT_VERSION);
    assert_eq!(entry.metrics["naming_clarity"].value, RawMetricValue::Scale(4.5));
    assert_eq!(entry.metrics["zero_based"].value, RawMetricValue::Scale(2.0));
    assert_eq!(
        entry.metrics["complexity_level"].value,
        RawMetricValue::Enum("medium".to_string())
    );
    // Binary confidence max(p, 1-p) = 0.95 becomes |2p - 1| = 0.9.
    assert!((entry.metrics["has_dead_code"].confidence - 0.9).abs() < 1e-9);

    // Upgrading an already-current entry must not shift it again.
    upgrade_cached_result(&mut entry, &metrics);
    assert_eq!(entry.metrics["naming_clarity"].value, RawMetricValue::Scale(4.5));
    assert!((entry.metrics["has_dead_code"].confidence - 0.9).abs() < 1e-9);
}

#[test]
fn test_v1_cache_entry_only_upgrades_binary_confidence() {
    let v1_json = r#"{
        "format_version": 1,
        "file_hash": "f",
        "metrics_hash": "m",
        "timestamp": "2026-09-22T10:00:00Z",
        "metrics": {
            "naming_clarity": { "value": 4.5, "confidence": 0.65 },
            "has_dead_code": { "value": true, "confidence": 0.5 }
        }
    }"#;
    let mut entry: CachedFileResult = serde_json::from_str(v1_json).unwrap();
    upgrade_cached_result(
        &mut entry,
        &[scale_metric("naming_clarity", [1.0, 5.0]), binary_metric("has_dead_code")],
    );

    assert_eq!(entry.metrics["naming_clarity"].value, RawMetricValue::Scale(4.5));
    assert_eq!(entry.metrics["has_dead_code"].confidence, 0.0);
}

#[test]
fn test_cache_write_creates_self_ignoring_state_dir() {
    let tmp = tempdir().unwrap();
    let result = CachedFileResult {
        format_version: CACHE_FORMAT_VERSION,
        file_hash: "f".to_string(),
        metrics_hash: "m".to_string(),
        timestamp: chrono::Utc::now(),
        metrics: HashMap::new(),
        usage: None,
    };

    put_cached_result(tmp.path(), "key", &result).unwrap();

    let gitignore = tmp.path().join(".qualitycheck").join(".gitignore");
    let content = std::fs::read_to_string(gitignore).unwrap();
    assert!(content.lines().any(|l| l.trim() == "*"));
}
