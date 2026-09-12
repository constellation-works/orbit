use serde_json::{Map, Value, json};

use super::super::test_support::{create_task, test_runtime};
use crate::adapter::command::ToolEntryPoint;
use orbit_types::task::TaskStatus;

const ENVELOPE_EXTRAS: &[&str] = &["warnings", "redactions", "redactions_applied"];

fn execute_as(
    runtime: &crate::OrbitRuntime,
    name: &str,
    input: Value,
    entry: ToolEntryPoint,
) -> Value {
    runtime
        .execute_tool_command_dispatch(
            name,
            input,
            Some("codex".to_string()),
            Some(orbit_common::test_fixtures::TEST_CODEX_MODEL.to_string()),
            entry,
        )
        .unwrap_or_else(|error| panic!("{name} via {entry:?} failed: {error}"))
        .value
}

fn execute(runtime: &crate::OrbitRuntime, name: &str, input: Value) -> Value {
    execute_as(runtime, name, input, ToolEntryPoint::Cli)
}

fn task_record_view(value: &Value) -> Map<String, Value> {
    let mut object = value
        .as_object()
        .unwrap_or_else(|| panic!("task envelope must be an object: {value}"))
        .clone();
    for extra in ENVELOPE_EXTRAS {
        object.remove(*extra);
    }
    object
}

fn envelope_keys(value: &Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .unwrap_or_else(|| panic!("task envelope must be an object: {value}"))
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn assert_same_task_record(left_label: &str, left: &Value, right_label: &str, right: &Value) {
    assert_eq!(
        task_record_view(left),
        task_record_view(right),
        "{left_label} and {right_label} must match field-for-field\n{left_label}: {left}\n{right_label}: {right}"
    );
}

#[test]
fn task_add_update_approve_start_match_show_and_cli_mcp_envelopes() {
    let (_root, runtime, repo_root) = test_runtime();

    let added_cli = execute_as(
        &runtime,
        "orbit.task.add",
        json!({
            "title": "Envelope parity CLI",
            "description": "Compare mutating tool output with task.show.",
            "complexity": "medium",
            "workspace": ".",
            "tags": ["envelope", "qa"],
        }),
        ToolEntryPoint::Cli,
    );
    let added_mcp = execute_as(
        &runtime,
        "orbit.task.add",
        json!({
            "title": "Envelope parity MCP",
            "description": "Compare mutating tool output with task.show.",
            "complexity": "medium",
            "workspace": ".",
            "tags": ["envelope", "qa"],
        }),
        ToolEntryPoint::Mcp,
    );
    assert_eq!(
        envelope_keys(&added_cli),
        envelope_keys(&added_mcp),
        "CLI and MCP add envelopes must advertise the same keys"
    );

    let added_id = added_cli["id"].as_str().expect("added id").to_string();
    let shown_cli = execute_as(
        &runtime,
        "orbit.task.show",
        json!({ "id": added_id }),
        ToolEntryPoint::Cli,
    );
    let shown_mcp = execute_as(
        &runtime,
        "orbit.task.show",
        json!({ "id": added_id }),
        ToolEntryPoint::Mcp,
    );
    assert_eq!(shown_cli, shown_mcp, "CLI and MCP show must be identical");
    assert_same_task_record("orbit.task.add", &added_cli, "orbit.task.show", &shown_cli);

    let updated = execute(
        &runtime,
        "orbit.task.update",
        json!({
            "id": added_id,
            "plan": "Keep the full record on every transport.",
            "comment": "envelope check",
        }),
    );
    let shown_after_update = execute(&runtime, "orbit.task.show", json!({ "id": added_id }));
    assert_same_task_record(
        "orbit.task.update",
        &updated,
        "orbit.task.show",
        &shown_after_update,
    );

    let proposed = create_task(
        &runtime,
        &repo_root,
        "Approve envelope",
        "Approve returns the full task.",
        TaskStatus::Proposed,
        &[],
    );
    let approved = execute(
        &runtime,
        "orbit.task.approve",
        json!({ "id": proposed.id, "note": "ready" }),
    );
    let shown_after_approve = execute(&runtime, "orbit.task.show", json!({ "id": proposed.id }));
    assert_same_task_record(
        "orbit.task.approve",
        &approved,
        "orbit.task.show",
        &shown_after_approve,
    );

    let ready = create_task(
        &runtime,
        &repo_root,
        "Start envelope",
        "Start returns the full task.",
        TaskStatus::Backlog,
        &[],
    );
    let started = execute(
        &runtime,
        "orbit.task.start",
        json!({ "id": ready.id, "note": "picking up" }),
    );
    let shown_after_start = execute(&runtime, "orbit.task.show", json!({ "id": ready.id }));
    assert_same_task_record(
        "orbit.task.start",
        &started,
        "orbit.task.show",
        &shown_after_start,
    );
}
