use serde_json::{Value, json};

use super::super::diagnostics::{
    is_year_month, list_jsonl_months, parse_jsonl_values, read_jsonl_month,
    validated_diagnostics_month_dir, validated_year_month,
};

#[test]
fn parse_jsonl_values_recovers_concatenated_objects() {
    let values = parse_jsonl_values::<Value>(r#"{"step":"one"}{"step":"two"}"#).unwrap();

    assert_eq!(values, vec![json!({"step": "one"}), json!({"step": "two"})]);
}

#[test]
fn parse_jsonl_values_rejects_trailing_garbage() {
    let err = parse_jsonl_values::<Value>(r#"{"step":"one"}oops"#).unwrap_err();

    assert!(err.to_string().contains("trailing characters"));
}

#[test]
fn is_year_month_accepts_only_canonical_form() {
    assert!(is_year_month("2026-03"));
    assert!(!is_year_month("2026-3"));
    assert!(!is_year_month("26-03"));
    assert!(!is_year_month("2026/03"));
    assert!(!is_year_month(""));
}

#[test]
fn validated_year_month_rebuilds_only_the_allowed_components() {
    assert_eq!(validated_year_month("2026-03").unwrap(), "2026-03");
    assert!(validated_year_month("2026/03").is_err());
    assert!(validated_year_month("../secrets").is_err());
}

#[test]
fn validated_month_dir_rejects_unknown_categories() {
    let root = tempfile::tempdir().expect("tempdir");

    let error = validated_diagnostics_month_dir(root.path(), "secrets", "2026-03").unwrap_err();

    assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));
}

#[test]
fn read_month_rejects_path_traversal() {
    let root = tempfile::tempdir().expect("tempdir");

    let error = read_jsonl_month::<Value>(root.path(), "metrics", "../secrets").unwrap_err();

    assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));
}

#[cfg(unix)]
#[test]
fn read_month_rejects_jsonl_symlink_outside_month() {
    let root = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let month_dir = root
        .path()
        .join("state")
        .join("diagnostics")
        .join("metrics")
        .join("2026-03");
    std::fs::create_dir_all(&month_dir).unwrap();
    let outside_file = outside.path().join("outside.jsonl");
    std::fs::write(&outside_file, r#"{"value":1}"#).unwrap();
    std::os::unix::fs::symlink(&outside_file, month_dir.join("entries.jsonl")).unwrap();

    let error = read_jsonl_month::<Value>(root.path(), "metrics", "2026-03").unwrap_err();

    assert!(matches!(error, orbit_common::OrbitError::InvalidInput(_)));
}

#[test]
fn list_jsonl_months_returns_sorted_existing_partitions() {
    let root = tempfile::tempdir().expect("tempdir");
    let category_dir = root
        .path()
        .join("state")
        .join("diagnostics")
        .join("metrics");
    std::fs::create_dir_all(category_dir.join("2026-01")).unwrap();
    std::fs::create_dir_all(category_dir.join("2025-12")).unwrap();
    std::fs::write(category_dir.join("not-a-month.txt"), "ignored").unwrap();

    let months = list_jsonl_months(root.path(), "metrics").unwrap();

    assert_eq!(months, vec!["2025-12".to_string(), "2026-01".to_string()]);
}

#[test]
fn list_jsonl_months_missing_category_dir_is_empty() {
    let root = tempfile::tempdir().expect("tempdir");

    let months = list_jsonl_months(root.path(), "metrics").unwrap();

    assert!(months.is_empty());
}
