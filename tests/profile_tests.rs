use qualitycheck::profile::{
    compute_active_metrics_hash, is_superseded_builtin, Profile, Rubric, BUILTIN_PROFILES,
};

const SUPERSEDED_FIXTURES: [&str; 3] = [
    include_str!("fixtures/superseded_profiles/quality-0.1.0.json"),
    include_str!("fixtures/superseded_profiles/security-0.1.0.json"),
    include_str!("fixtures/superseded_profiles/qa-0.1.0.json"),
];

fn profile_with_metric(metric_json: &str) -> String {
    format!(
        r#"{{ "name": "t", "description": "d", "fail_below": 3.0, "metrics": [{metric_json}] }}"#
    )
}

fn validation_error(metric_json: &str) -> String {
    Profile::validate_and_parse(&profile_with_metric(metric_json), "t")
        .expect_err("profile should be rejected")
        .to_string()
}

#[test]
fn test_builtin_profiles_parse_with_rubrics() {
    for (name, content) in BUILTIN_PROFILES {
        let profile = Profile::validate_and_parse(content, name).unwrap();
        for metric in &profile.metrics {
            assert!(metric.rubric.is_some(), "{name}.{} has no rubric", metric.id);
        }
    }
}

#[test]
fn test_superseded_builtin_detection() {
    for fixture in SUPERSEDED_FIXTURES {
        assert!(is_superseded_builtin(fixture));
        assert!(is_superseded_builtin(&fixture.replace('\n', "\r\n")));
        // Any user edit makes it the user's profile.
        assert!(!is_superseded_builtin(&fixture.replace("\"fail_below\"", "\"fail_below\" ")));
    }
    for (_, current) in BUILTIN_PROFILES {
        assert!(!is_superseded_builtin(current));
    }
}

#[test]
fn test_scale_rubric_must_match_level_count() {
    let err = validation_error(
        r#"{ "id": "m", "type": "scale", "range": [1, 3], "question": "q", "weight": 1,
             "rubric": ["low", "high"] }"#,
    );
    assert!(err.contains("exactly 3 level descriptions"), "{err}");
}

#[test]
fn test_scale_range_limited_to_jev_level_count() {
    let err = validation_error(
        r#"{ "id": "m", "type": "scale", "range": [0, 10], "question": "q", "weight": 1 }"#,
    );
    assert!(err.contains("11 levels"), "{err}");
}

#[test]
fn test_enum_rubric_must_describe_exactly_the_options() {
    let err = validation_error(
        r#"{ "id": "m", "type": "enum", "options": ["low", "high"], "question": "q", "weight": 1,
             "rubric": { "low": "a", "medium": "b" } }"#,
    );
    assert!(err.contains("one description per option"), "{err}");
}

#[test]
fn test_binary_rubric_must_describe_true_and_false() {
    let err = validation_error(
        r#"{ "id": "m", "type": "binary", "question": "q", "weight": 1,
             "rubric": { "yes": "a", "no": "b" } }"#,
    );
    assert!(err.contains("\"true\" and \"false\""), "{err}");

    let ok = profile_with_metric(
        r#"{ "id": "m", "type": "binary", "question": "q", "weight": 1,
             "rubric": { "true": "a", "false": "b" }, "applies_when": "c" }"#,
    );
    let profile = Profile::validate_and_parse(&ok, "t").unwrap();
    assert!(matches!(profile.metrics[0].rubric, Some(Rubric::Descriptions(_))));
    assert_eq!(profile.metrics[0].applies_when.as_deref(), Some("c"));
}

#[test]
fn test_metrics_hash_tracks_questions_not_scoring_policy() {
    let base = Profile::validate_and_parse(BUILTIN_PROFILES[0].1, "quality").unwrap();
    let hash = |p: &Profile| compute_active_metrics_hash(std::slice::from_ref(p));

    let mut reweighted = base.clone();
    reweighted.metrics[0].weight = 9.0;
    reweighted.min_confidence = Some(0.9);
    reweighted.fail_below = 4.5;
    assert_eq!(hash(&base), hash(&reweighted), "policy changes must reuse cached answers");

    let mut reworded = base.clone();
    reworded.metrics[0].applies_when = Some("something else".to_string());
    assert_ne!(hash(&base), hash(&reworded), "question changes must invalidate the cache");
}

/// Pins the shipped built-in profiles. Users' unedited installed copies of a built-in only keep
/// upgrading if every shipped version's hash is listed in `SUPERSEDED_BUILTIN_HASHES`.
const CURRENT_BUILTIN_HASHES: [(&str, &str); 3] = [
    ("quality", "3e44d5adb7debc3ac6ce00bd3485b9bf2ab6ed82d08ff2d88a32d81d60896f53"),
    ("security", "c87cb796955094046fdf16739ba9a58ac3b77377f4f3738c1db2f10c72e0477b"),
    ("qa", "4ce888d08b13c2c8f492b051f49cd57cd83c4ec86d7e59af39aaa352c075d50c"),
];

#[test]
fn test_changed_builtin_profile_records_superseded_hash() {
    for ((name, content), (pinned_name, pinned_hash)) in BUILTIN_PROFILES.iter().zip(CURRENT_BUILTIN_HASHES) {
        assert_eq!(*name, pinned_name);
        let hash = blake3::hash(content.replace("\r\n", "\n").as_bytes()).to_hex().to_string();
        assert_eq!(
            hash, pinned_hash,
            "built-in profile '{name}' changed. Add its previous hash ({pinned_hash}) to \
             SUPERSEDED_BUILTIN_HASHES in src/profile.rs so unedited installed copies keep \
             upgrading, then pin the new hash ({hash}) here."
        );
    }
}
