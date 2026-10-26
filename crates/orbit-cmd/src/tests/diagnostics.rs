use serde_json::Value;

use super::super::diagnostics::{list_jsonl_months, read_jsonl_month, read_jsonl_month_limited};

fn write_month(root: &std::path::Path, bytes: &[u8]) {
    let month_dir = root
        .join("state")
        .join("diagnostics")
        .join("metrics")
        .join("2026-03");
    std::fs::create_dir_all(&month_dir).unwrap();
    std::fs::write(month_dir.join("entries.jsonl"), bytes).unwrap();
}

#[test]
fn read_month_skips_non_utf8_line_and_returns_remaining_entries() {
    let root = tempfile::tempdir().expect("tempdir");
    // Valid record, a line truncated mid-character (invalid UTF-8), valid record.
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"{\"value\":1}\n");
    bytes.extend_from_slice(b"{\"value\":2}\xe2\x80\n");
    bytes.extend_from_slice(b"{\"value\":3}\n");
    write_month(root.path(), &bytes);

    let full = read_jsonl_month::<Value>(root.path(), "metrics", "2026-03").unwrap();
    let full_values: Vec<i64> = full
        .iter()
        .map(|entry| entry["value"].as_i64().unwrap())
        .collect();
    assert_eq!(full_values, [1, 3], "full reader must skip the bad line");

    let limited = read_jsonl_month_limited::<Value>(root.path(), "metrics", "2026-03", 10).unwrap();
    let limited_values: Vec<i64> = limited
        .iter()
        .map(|entry| entry["value"].as_i64().unwrap())
        .collect();
    assert_eq!(
        limited_values,
        [3, 1],
        "limited reader must skip the bad line and keep newest-first order"
    );
}

#[test]
fn list_months_skips_names_the_reader_rejects() {
    let root = tempfile::tempdir().expect("tempdir");
    for name in ["2026-03", "2026-00", "2026-13", "2026-12"] {
        std::fs::create_dir_all(
            root.path()
                .join("state")
                .join("diagnostics")
                .join("metrics")
                .join(name),
        )
        .unwrap();
    }

    let months = list_jsonl_months(root.path(), "metrics").unwrap();

    assert_eq!(months, ["2026-03", "2026-12"]);
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
