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
    let mut fields = err.wire_details();
    fields.insert("code".into(), json!(err.wire_code()));
    fields.insert("message".into(), json!(err.to_string()));
    Value::Object(fields)
}
