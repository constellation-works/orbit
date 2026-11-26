use serde_json::Value;

use crate::command::{CommandOut, Payload};

pub(super) fn record_payload(value: &Value) -> CommandOut {
    if !value.is_object() {
        return Ok(Payload::document(value.clone()).into());
    }

    let mut lines = vec![
        format!("ID: {}", value_string(value, "id")),
        format!("Status: {}", value_string(value, "status")),
        format!("Model: {}", value_string(value, "model")),
    ];
    let tags = value_string_list(value, "tags");
    if !tags.is_empty() {
        lines.push(format!("Tags: {tags}"));
    }
    let task = value_string(value, "during_task");
    if !task.is_empty() {
        lines.push(format!("Task: {task}"));
    }
    let rehome_to = value_string(value, "rehome_to");
    if !rehome_to.is_empty() {
        lines.push(format!("Re-home to: {rehome_to}"));
    }
    // `orbit friction rehome` answers with the resolved source record plus
    // the copy it created in the owning workspace.
    if let Some(moved) = value.get("rehomed_as") {
        lines.push(format!("Re-homed as: {}", value_string(moved, "id")));
    }
    let dropped = value_string_list(value, "dropped_tags");
    if !dropped.is_empty() {
        lines.push(format!("Dropped tags: {dropped}"));
    }
    // ADR-0345: `path` is the legacy evidence file an imported record came
    // from. Records written after the SQLite cutover carry `null` and render
    // no row rather than a location nothing could open.
    let path = value_string(value, "path");
    if !path.is_empty() {
        lines.push(format!("Legacy file: {path}"));
    }
    let body = value_string(value, "body");
    if !body.is_empty() {
        lines.push(format!("\n{body}"));
    }
    Ok(Payload::detail(value.clone(), lines.join("\n")).into())
}

pub(super) fn value_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(super) fn value_string_list(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
