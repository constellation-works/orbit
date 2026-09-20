use serde_json::json;

use super::super::input::{
    parse_artifacts, parse_optional_string_array_field, parse_string_array_field,
};

#[test]
fn parse_string_array_field_accepts_scalar_string() {
    assert_eq!(
        parse_string_array_field(&json!({"run_ids":"run-1"}), "run_ids").unwrap(),
        vec!["run-1"]
    );
}

#[test]
fn parse_string_array_field_preserves_array_behavior() {
    assert_eq!(
        parse_string_array_field(&json!({"run_ids":["run-1", "run-2"]}), "run_ids").unwrap(),
        vec!["run-1", "run-2"]
    );
}

#[test]
fn parse_string_array_field_rejects_non_string_shapes() {
    let error = parse_string_array_field(&json!({"run_ids":{"id":"run-1"}}), "run_ids")
        .unwrap_err()
        .to_string();
    assert!(error.contains("`run_ids` must be a string or array of strings"));
}

#[test]
fn parse_optional_string_array_field_treats_empty_values_as_unrestricted() {
    assert_eq!(
        parse_optional_string_array_field(&json!({}), "allowed_crews").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        parse_optional_string_array_field(&json!({"allowed_crews": null}), "allowed_crews")
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        parse_optional_string_array_field(&json!({"allowed_crews": []}), "allowed_crews").unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn parse_artifacts_accepts_text_and_byte_array_content() {
    let artifacts = parse_artifacts(&json!({
        "artifacts": [
            {"path": "notes.txt", "content": "hello"},
            {"path": "image.bin", "media_type": "application/octet-stream", "content": [0, 159, 255]}
        ]
    }))
    .unwrap();

    assert_eq!(artifacts[0].text_content(), Some("hello"));
    assert_eq!(artifacts[0].media_type, "text/plain");
    assert_eq!(artifacts[1].content, vec![0, 159, 255]);
    assert_eq!(artifacts[1].media_type, "application/octet-stream");
}

#[test]
fn parse_artifacts_rejects_invalid_byte_content() {
    let error = parse_artifacts(&json!({
        "artifacts": [{"path": "image.bin", "content": [256]}]
    }))
    .unwrap_err()
    .to_string();

    assert!(error.contains("between 0 and 255"));
}
