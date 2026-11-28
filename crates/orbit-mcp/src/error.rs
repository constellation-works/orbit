//! Translation of `OrbitError` values into structured MCP tool error payloads; imports `orbit-common` only.

use orbit_common::{McpErrorSurface, OrbitError};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

/// Map an [`OrbitError`] into a structured MCP error result.
pub(crate) fn tool_error_result(err: &OrbitError) -> CallToolResult {
    CallToolResult::structured_error(error_payload_for_surface(err, McpErrorSurface::default()))
}

/// Render recovery hints against the tools this session advertises and can execute.
pub(crate) fn tool_error_result_for_surface(
    err: &OrbitError,
    surface: McpErrorSurface,
) -> CallToolResult {
    let mut payload = error_payload_for_surface(err, surface);
    if let Some(message) = payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        let mut message = message;
        if !surface.task_show {
            for hint in [
                "`orbit_task_show` and `field: \"artifacts\"`",
                "`orbit_task_show` with `field: \"artifacts\"`",
            ] {
                message = message.replace(hint, "an operator to list available artifacts");
            }
        }
        if !surface.search
            && message.contains("; to read or list friction records, use `orbit_search`")
            && let Some((prefix, _)) = message.split_once("; to read or list friction records")
        {
            message = format!("{prefix}; ask an operator to read or list friction records");
        }
        payload["message"] = json!(message);
    }
    CallToolResult::structured_error(payload)
}

fn error_payload_for_surface(err: &OrbitError, surface: McpErrorSurface) -> Value {
    if let OrbitError::RemoteTool { payload, .. } = err {
        return payload.clone();
    }
    let stored = err
        .mcp_error_message_for_surface(surface)
        .replace(
            "`orbit.task.show` with `fields: [\"artifacts\"]`",
            "`orbit_task_show` with `field: \"artifacts\"`",
        )
        .replace(
            "`orbit.search` with kind `friction`",
            "`orbit_search` with kind `friction`",
        )
        .replace(
            "`orbit.task.artifact` action",
            "`orbit_task_artifact` action",
        )
        .replace("`orbit.friction` action", "`orbit_friction` action");
    let reader = orbit_common::source_repair_reader_text(&stored);
    let shown = reader
        .as_ref()
        .map(|text| text.line.as_str())
        .unwrap_or(stored.as_str());
    let mut fields = err.wire_details();
    fields.insert("code".into(), json!(err.wire_code()));
    fields.insert("message".into(), json!(shown));
    if let Some(text) = reader.filter(|text| text.line != text.detail) {
        fields.insert("detail".into(), json!(text.detail));
    }
    Value::Object(fields)
}
