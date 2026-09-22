//! The versioned response envelope (§4.2) and `output_schema` validation.
//!
//! There is no partial success: anything short of `{"ok": true, "output":
//! <valid>}` is a tool error naming the cause, and the caller never sees
//! the backend's bytes.

use orbit_common::OrbitError;
use serde_json::Value;

use super::schema::CompiledSchema;

/// The stdin envelope version the backend receives.
pub const PLUGIN_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Turn the backend's stdout into its `output`, or the error it reported.
pub fn parse_response(tool_name: &str, stdout: &str) -> Result<Value, OrbitError> {
    let response: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
        OrbitError::Execution(format!(
            "plugin tool '{tool_name}' produced invalid JSON output: {error}"
        ))
    })?;
    match response.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(response.get("output").cloned().unwrap_or(Value::Null)),
        Some(false) => {
            let error = response.get("error").cloned().unwrap_or(Value::Null);
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("plugin_error");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the plugin reported a failure without a message");
            Err(OrbitError::Execution(format!(
                "plugin tool '{tool_name}' failed ({code}): {message}"
            )))
        }
        None => Err(OrbitError::Execution(format!(
            "plugin tool '{tool_name}' returned an envelope without a boolean `ok`"
        ))),
    }
}

/// Check `output` against the tool's `output_schema`, when it declares one.
///
/// The validator was compiled when the plugin was loaded, so a schema that
/// cannot compile never reaches a call (§4.9).
pub fn validate_output(
    tool_name: &str,
    output_schema: Option<&CompiledSchema>,
    output: &Value,
) -> Result<(), OrbitError> {
    let Some(schema) = output_schema else {
        return Ok(());
    };
    if let Some(details) = schema.violations(output) {
        return Err(OrbitError::Execution(format!(
            "plugin tool '{tool_name}' returned output that violates its output_schema: {details}"
        )));
    }
    Ok(())
}
