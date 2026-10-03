use serde_json::Value;

use super::super::diagnostics::read_jsonl_month;

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
