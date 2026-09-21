use std::collections::HashMap;

use qualitycheck::cache::{CachedMetricResult, RawMetricValue};
use qualitycheck::profile::{Metric, MetricType, Profile};
use qualitycheck::scorer::{evaluate_file_with_metrics, normalize_metric_score};

#[test]
fn test_normalize_scale_metric() {
    let metric = Metric {
        id: "naming_clarity".to_string(),
        metric_type: MetricType::Scale,
        question: "Are names clear?".to_string(),
        weight: 1.0,
        range: Some([1.0, 5.0]),
        options: None,
        good_value: None,
        score_map: None,
    };

    assert_eq!(normalize_metric_score(&metric, &RawMetricValue::Scale(1.0)), 1.0);
    assert_eq!(normalize_metric_score(&metric, &RawMetricValue::Scale(3.0)), 3.0);
    assert_eq!(normalize_metric_score(&metric, &RawMetricValue::Scale(5.0)), 5.0);
    assert_eq!(normalize_metric_score(&metric, &RawMetricValue::Scale(4.0)), 4.0);

    // Test different range [0, 10] normalized to [1, 5]
    let metric_10 = Metric {
        id: "scale_10".to_string(),
        metric_type: MetricType::Scale,
        question: "Rate 0 to 10".to_string(),
        weight: 1.0,
        range: Some([0.0, 10.0]),
        options: None,
        good_value: None,
        score_map: None,
    };

    assert_eq!(normalize_metric_score(&metric_10, &RawMetricValue::Scale(0.0)), 1.0);
    assert_eq!(normalize_metric_score(&metric_10, &RawMetricValue::Scale(5.0)), 3.0);
    assert_eq!(normalize_metric_score(&metric_10, &RawMetricValue::Scale(10.0)), 5.0);
}

#[test]
fn test_normalize_binary_metric() {
    let metric_defect = Metric {
        id: "has_dead_code".to_string(),
        metric_type: MetricType::Binary,
        question: "Does it have dead code?".to_string(),
        weight: 0.5,
        range: None,
        options: None,
        good_value: Some(false), // default for defect check
        score_map: None,
    };

    // false means no defect -> good (5.0)
    assert_eq!(normalize_metric_score(&metric_defect, &RawMetricValue::Binary(false)), 5.0);
    // true means defect exists -> bad (1.0)
    assert_eq!(normalize_metric_score(&metric_defect, &RawMetricValue::Binary(true)), 1.0);

    let metric_positive = Metric {
        id: "has_tests".to_string(),
        metric_type: MetricType::Binary,
        question: "Has tests?".to_string(),
        weight: 1.0,
        range: None,
        options: None,
        good_value: Some(true),
        score_map: None,
    };

    assert_eq!(normalize_metric_score(&metric_positive, &RawMetricValue::Binary(true)), 5.0);
    assert_eq!(normalize_metric_score(&metric_positive, &RawMetricValue::Binary(false)), 1.0);
}

#[test]
fn test_normalize_enum_metric() {
    let metric_complexity = Metric {
        id: "complexity_level".to_string(),
        metric_type: MetricType::Enum,
        question: "Complexity level?".to_string(),
        weight: 1.5,
        range: None,
        options: Some(vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "critical".to_string(),
        ]),
        good_value: None,
        score_map: None,
    };

    // Lower is better: low -> 5.0, critical -> 1.0
    assert_eq!(
        normalize_metric_score(&metric_complexity, &RawMetricValue::Enum("low".to_string())),
        5.0
    );
    assert_eq!(
        normalize_metric_score(&metric_complexity, &RawMetricValue::Enum("critical".to_string())),
        1.0
    );

    let metric_validation = Metric {
        id: "input_validation".to_string(),
        metric_type: MetricType::Enum,
        question: "Input validation?".to_string(),
        weight: 1.5,
        range: None,
        options: Some(vec![
            "poor".to_string(),
            "fair".to_string(),
            "good".to_string(),
            "high".to_string(),
        ]),
        good_value: None,
        score_map: None,
    };

    // Higher is better: poor -> 1.0, high -> 5.0
    assert_eq!(
        normalize_metric_score(&metric_validation, &RawMetricValue::Enum("poor".to_string())),
        1.0
    );
    assert_eq!(
        normalize_metric_score(&metric_validation, &RawMetricValue::Enum("high".to_string())),
        5.0
    );
}

#[test]
fn test_composite_score_calculation() {
    let profile = Profile {
        schema: None,
        name: "quality".to_string(),
        description: "Code quality".to_string(),
        fail_below: 3.0,
        metrics: vec![
            Metric {
                id: "naming_clarity".to_string(),
                metric_type: MetricType::Scale,
                question: "Naming clarity".to_string(),
                weight: 1.0,
                range: Some([1.0, 5.0]),
                options: None,
                good_value: None,
                score_map: None,
            },
            Metric {
                id: "has_dead_code".to_string(),
                metric_type: MetricType::Binary,
                question: "Dead code".to_string(),
                weight: 0.5,
                range: None,
                options: None,
                good_value: Some(false),
                score_map: None,
            },
            Metric {
                id: "complexity_level".to_string(),
                metric_type: MetricType::Enum,
                question: "Complexity".to_string(),
                weight: 1.5,
                range: None,
                options: Some(vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "critical".to_string(),
                ]),
                good_value: None,
                score_map: None,
            },
        ],
    };

    let mut results = HashMap::new();
    // naming_clarity = 4.0 (wt 1.0) -> 4.0
    results.insert(
        "naming_clarity".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Scale(4.0),
            confidence: 0.87,
        },
    );
    // has_dead_code = false (wt 0.5) -> 5.0
    results.insert(
        "has_dead_code".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Binary(false),
            confidence: 0.95,
        },
    );
    // complexity_level = "medium" (wt 1.5) -> 5.0 - (1/3)*4.0 = 3.6666...
    results.insert(
        "complexity_level".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Enum("medium".to_string()),
            confidence: 0.90,
        },
    );

    let evals = evaluate_file_with_metrics(&[profile], &results);
    assert_eq!(evals.len(), 1);
    let eval = &evals[0];

    // Expected composite score:
    // (4.0 * 1.0 + 5.0 * 0.5 + 3.6666... * 1.5) / (1.0 + 0.5 + 1.5)
    // = (4.0 + 2.5 + 5.5) / 3.0 = 12.0 / 3.0 = 4.0
    assert_eq!(eval.composite_score, 4.0);
    assert!(eval.passed);
}
