use serde_json::json;

use crate::protocol::tool_input::{
    reject_retired_task_add_input_fields, reject_unknown_tool_fields,
};

#[test]
fn reject_unknown_tool_fields_suggests_comment_for_note() {
    let error = reject_unknown_tool_fields(
        &json!({ "id": "ORB-1", "note": "should be comment" }),
        &["id", "comment", "status"],
    )
    .expect_err("unknown field must fail");

    let message = error.to_string();
    assert!(message.contains("unknown field 'note'"), "{message}");
    assert!(message.contains("did you mean 'comment'"), "{message}");
    assert_eq!(
        error.did_you_mean().map(|names| names.to_vec()),
        Some(vec!["comment".to_string()])
    );
}

#[test]
fn reject_unknown_tool_fields_normalizes_camel_case() {
    let error = reject_unknown_tool_fields(
        &json!({ "acceptanceCriteria": ["one"] }),
        &["acceptance_criteria", "title"],
    )
    .expect_err("camelCase alias must fail with a canonical hint");

    assert!(
        error
            .to_string()
            .contains("did you mean 'acceptance_criteria'"),
        "{}",
        error
    );
}

#[test]
fn reject_unknown_tool_fields_allows_transport_wrappers() {
    reject_unknown_tool_fields(
        &json!({
            "id": "ORB-1",
            "workspace": "ws_orbit",
            "_meta": { "orbit": { "workspace": "ws_orbit" } }
        }),
        &["id"],
    )
    .expect("transport wrappers are not tool arguments");
}

#[test]
fn reject_retired_task_add_fields_points_at_update() {
    let error = reject_retired_task_add_input_fields(&json!({
        "title": "x",
        "dependencies": ["ORB-1"],
    }))
    .expect_err("retired add fields must fail");

    let message = error.to_string();
    assert!(
        message.contains("unknown field 'dependencies'"),
        "{message}"
    );
    assert!(message.contains("orbit.task.update"), "{message}");
}
