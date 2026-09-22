use std::collections::HashMap;

use qualitycheck::cache::{CachedMetricResult, RawMetricValue};
use qualitycheck::profile::{Metric, MetricType, Profile};
use qualitycheck::scorer::{
    evaluate_file_with_metrics, find_regressions, normalize_metric_score, score_metric_answer,
    FileEvaluation,
    ProfileEvaluation, RegressionKind, RunUsage, ScanRunResult, SCORING_VERSION,
};

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
        rubric: None,
        applies_when: None,
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
        rubric: None,
        applies_when: None,
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
        rubric: None,
        applies_when: None,
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
        rubric: None,
        applies_when: None,
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
        rubric: None,
        applies_when: None,
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
        rubric: None,
        applies_when: None,
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
        min_confidence: None,
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
                rubric: None,
                applies_when: None,
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
                rubric: None,
                applies_when: None,
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
                rubric: None,
                applies_when: None,
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
            probabilities: None,
        },
    );
    // has_dead_code = false (wt 0.5) -> 5.0
    results.insert(
        "has_dead_code".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Binary(false),
            confidence: 0.95,
            probabilities: None,
        },
    );
    // complexity_level = "medium" (wt 1.5) -> 5.0 - (1/3)*4.0 = 3.6666...
    results.insert(
        "complexity_level".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Enum("medium".to_string()),
            confidence: 0.90,
            probabilities: None,
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

fn gating_profile(min_confidence: Option<f64>) -> Profile {
    let scale = |id: &str| Metric {
        id: id.to_string(),
        metric_type: MetricType::Scale,
        question: id.to_string(),
        weight: 1.0,
        range: Some([1.0, 5.0]),
        options: None,
        good_value: None,
        score_map: None,
        rubric: None,
        applies_when: None,
    };
    Profile {
        schema: None,
        name: "quality".to_string(),
        description: "Code quality".to_string(),
        fail_below: 3.0,
        min_confidence,
        metrics: vec![scale("naming_clarity"), scale("cohesion")],
    }
}

fn answers(naming: (f64, f64), cohesion: (f64, f64)) -> HashMap<String, CachedMetricResult> {
    let mut results = HashMap::new();
    for (id, (value, confidence)) in [("naming_clarity", naming), ("cohesion", cohesion)] {
        results.insert(
            id.to_string(),
            CachedMetricResult {
                value: RawMetricValue::Scale(value),
                confidence,
                probabilities: None,
            },
        );
    }
    results
}

#[test]
fn test_low_confidence_metric_is_excluded_from_composite() {
    // cohesion = 1 at 5% confidence would drag the composite to 2.5 and fail the file.
    let evals = evaluate_file_with_metrics(&[gating_profile(None)], &answers((4.0, 0.9), (1.0, 0.05)));
    let eval = &evals[0];

    assert_eq!(eval.composite_score, 4.0);
    assert!(eval.passed);
    assert!(!eval.inconclusive);
    assert_eq!(eval.min_confidence, 0.2);
    let cohesion = eval.metrics.iter().find(|m| m.metric_id == "cohesion").unwrap();
    assert!(cohesion.excluded_low_confidence);
    let naming = eval.metrics.iter().find(|m| m.metric_id == "naming_clarity").unwrap();
    assert!(!naming.excluded_low_confidence);
}

#[test]
fn test_all_metrics_below_min_confidence_is_inconclusive_not_failed() {
    let evals = evaluate_file_with_metrics(&[gating_profile(None)], &answers((1.0, 0.1), (2.0, 0.0)));
    let eval = &evals[0];

    assert!(eval.inconclusive);
    assert!(eval.passed, "an unjudged profile must not fail --strict");
    // Reported composite falls back to all metrics, for reference.
    assert_eq!(eval.composite_score, 1.5);
}

#[test]
fn test_profile_min_confidence_override() {
    let low_conf = answers((4.0, 0.9), (1.0, 0.05));

    let counted = evaluate_file_with_metrics(&[gating_profile(Some(0.0))], &low_conf);
    assert_eq!(counted[0].composite_score, 2.5);
    assert!(!counted[0].passed);

    let strict = evaluate_file_with_metrics(&[gating_profile(Some(0.95))], &low_conf);
    assert!(strict[0].inconclusive);
}

#[test]
fn test_not_applicable_metric_is_excluded_from_composite() {
    let mut profile = gating_profile(None);
    profile.metrics[1].applies_when = Some("The file contains its own logic.".to_string());

    let mut results = answers((4.0, 0.9), (1.0, 0.9));
    results.insert(
        "cohesion:applies".to_string(),
        CachedMetricResult {
            value: RawMetricValue::Binary(false),
            confidence: 0.8,
            probabilities: None,
        },
    );
    let evals = evaluate_file_with_metrics(std::slice::from_ref(&profile), &results);
    let cohesion = evals[0].metrics.iter().find(|m| m.metric_id == "cohesion").unwrap();
    assert!(cohesion.not_applicable);
    assert_eq!(evals[0].composite_score, 4.0);

    // Judged applicable: it counts.
    results.get_mut("cohesion:applies").unwrap().value = RawMetricValue::Binary(true);
    let evals = evaluate_file_with_metrics(&[profile], &results);
    assert!(!evals[0].metrics[1].not_applicable);
    assert_eq!(evals[0].composite_score, 2.5);
}

/// A run with one "quality" profile (fail_below 3.0) per file: (path, composite, inconclusive).
fn run_of(files: &[(&str, f64, bool)]) -> ScanRunResult {
    ScanRunResult {
        scoring_version: SCORING_VERSION,
        run_id: "r".to_string(),
        timestamp: chrono::Utc::now(),
        target_path: ".".to_string(),
        total_files: files.len(),
        cached_files: 0,
        profiles_used: vec!["quality".to_string()],
        files: files
            .iter()
            .map(|(path, composite, inconclusive)| FileEvaluation {
                path: path.into(),
                relative_path: path.to_string(),
                served_from_cache: false,
                usage: None,
                context_files: Vec::new(),
                profiles: vec![ProfileEvaluation {
                    profile_name: "quality".to_string(),
                    composite_score: *composite,
                    fail_below: 3.0,
                    min_confidence: 0.2,
                    passed: *inconclusive || *composite >= 3.0,
                    inconclusive: *inconclusive,
                    metrics: Vec::new(),
                }],
            })
            .collect(),
        usage: RunUsage::default(),
        passed: true,
        exit_reason: None,
    }
}

#[test]
fn test_find_regressions_reports_only_what_the_change_made_worse() {
    let base = run_of(&[
        ("dropped.rs", 4.5, false),
        ("within_allowance.rs", 4.5, false),
        ("already_low.rs", 2.0, false),
        ("was_inconclusive.rs", 4.0, true),
    ]);
    let head = run_of(&[
        ("dropped.rs", 3.8, false),
        ("within_allowance.rs", 4.0, false),
        ("already_low.rs", 1.9, false),
        ("was_inconclusive.rs", 2.5, false),
        ("new_low.rs", 2.9, false),
        ("new_ok.rs", 3.1, false),
        ("new_inconclusive.rs", 1.0, true),
    ]);

    let regressions = find_regressions(&base, &head, 0.5);
    let found: Vec<(&str, RegressionKind)> = regressions
        .iter()
        .map(|r| (r.relative_path.as_str(), r.kind))
        .collect();
    assert_eq!(
        found,
        vec![
            ("dropped.rs", RegressionKind::Dropped),
            ("was_inconclusive.rs", RegressionKind::BelowThresholdWithoutBase),
            ("new_low.rs", RegressionKind::BelowThresholdWithoutBase),
        ]
    );
    assert_eq!(regressions[0].old_composite, Some(4.5));
    assert_eq!(regressions[0].new_composite, 3.8);

    // A drop exactly equal to the allowance is allowed; zero allowance flags any drop.
    assert!(find_regressions(&base, &head, 0.7)
        .iter()
        .all(|r| r.relative_path != "dropped.rs"));
    assert!(find_regressions(&base, &head, 0.0)
        .iter()
        .any(|r| r.relative_path == "already_low.rs"));
}

fn enum_answer(choice: &str, distribution: &[(&str, f64)]) -> CachedMetricResult {
    CachedMetricResult {
        value: RawMetricValue::Enum(choice.to_string()),
        confidence: 0.3,
        probabilities: Some(distribution.iter().map(|(o, p)| (o.to_string(), *p)).collect()),
    }
}

fn input_validation_metric() -> Metric {
    Metric {
        id: "input_validation".to_string(),
        metric_type: MetricType::Enum,
        question: "Input validation?".to_string(),
        weight: 1.5,
        range: None,
        options: Some(["poor", "fair", "good", "high"].map(String::from).to_vec()),
        good_value: None,
        score_map: None,
        rubric: None,
        applies_when: None,
    }
}

#[test]
fn test_near_tie_between_options_does_not_swing_the_score() {
    // Near-identical code answered "good" by a hair, then "fair" by a hair (as observed on a
    // real controller at ~0.32 confidence). Scoring the top pick alone swings 1.3 points.
    let metric = input_validation_metric();
    let base = enum_answer("good", &[("poor", 0.05), ("fair", 0.40), ("good", 0.45), ("high", 0.10)]);
    let head = enum_answer("fair", &[("poor", 0.05), ("fair", 0.45), ("good", 0.40), ("high", 0.10)]);

    let top_pick_swing = normalize_metric_score(&metric, &base.value) - normalize_metric_score(&metric, &head.value);
    assert!(top_pick_swing > 1.3);

    let swing = score_metric_answer(&metric, &base) - score_metric_answer(&metric, &head);
    assert!(swing.abs() < 0.1, "expected-value swing was {swing}");
}

#[test]
fn test_enum_answer_scores_as_probability_weighted_mean() {
    let metric = input_validation_metric();
    // poor=1, fair=2.33, good=3.67, high=5.
    let split = enum_answer("good", &[("good", 0.5), ("high", 0.5)]);
    assert!((score_metric_answer(&metric, &split) - (3.0 + 2.0 / 3.0 + 5.0) / 2.0).abs() < 1e-9);

    // Option keys match case-insensitively; probabilities are renormalized over known options.
    let partial = enum_answer("high", &[("HIGH", 0.6), ("unknown", 0.4)]);
    assert!((score_metric_answer(&metric, &partial) - 5.0).abs() < 1e-9);

    // Without a distribution the chosen option is scored alone.
    let bare = CachedMetricResult {
        value: RawMetricValue::Enum("fair".to_string()),
        confidence: 0.9,
        probabilities: None,
    };
    assert!((score_metric_answer(&metric, &bare) - (1.0 + 4.0 / 3.0)).abs() < 1e-9);
}

#[test]
fn test_binary_answer_scores_by_probability_of_the_good_outcome() {
    let dead_code = Metric {
        id: "has_dead_code".to_string(),
        metric_type: MetricType::Binary,
        question: "Dead code?".to_string(),
        weight: 0.5,
        range: None,
        options: None,
        good_value: Some(false),
        score_map: None,
        rubric: None,
        applies_when: None,
    };
    let answer = |p_true: f64| CachedMetricResult {
        value: RawMetricValue::Binary(p_true >= 0.5),
        confidence: (2.0 * p_true - 1.0).abs(),
        probabilities: Some(qualitycheck::cache::binary_probabilities(p_true)),
    };

    assert!((score_metric_answer(&dead_code, &answer(0.05)) - 4.8).abs() < 1e-9);
    assert!((score_metric_answer(&dead_code, &answer(0.5)) - 3.0).abs() < 1e-9);
    // Just either side of the verdict boundary: nearly the same score, not 5 vs 1.
    let swing = score_metric_answer(&dead_code, &answer(0.49)) - score_metric_answer(&dead_code, &answer(0.51));
    assert!(swing.abs() < 0.1);
}
