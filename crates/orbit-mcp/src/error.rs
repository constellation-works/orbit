use orbit_common::OrbitError;
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

/// Map an [`OrbitError`] into a structured MCP error result.
pub(crate) fn tool_error_result(err: &OrbitError) -> CallToolResult {
    CallToolResult::structured_error(error_payload(err))
}

fn error_payload(err: &OrbitError) -> Value {
    if let OrbitError::RemoteTool { payload, .. } = err {
        return payload.clone();
    }
    let stored = err.to_string();
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
