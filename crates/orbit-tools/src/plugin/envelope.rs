//! The versioned response envelope (§4.2) and `output_schema` validation.
//!
//! There is no partial success: anything short of `{"ok": true, "output":
//! <valid>}` is a tool error naming the cause, and the caller never sees
//! the backend's bytes.

use orbit_common::OrbitError;
use serde_json::Value;

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
pub fn validate_output(
    tool_name: &str,
    output_schema: Option<&Value>,
    output: &Value,
) -> Result<(), OrbitError> {
    let Some(schema) = output_schema else {
        return Ok(());
    };
    let validator = jsonschema::JSONSchema::compile(schema).map_err(|error| {
        OrbitError::Execution(format!(
            "plugin tool '{tool_name}' declares an output_schema that does not compile: {error}"
        ))
    })?;
    if let Err(errors) = validator.validate(output) {
        let details = errors
            .map(|error| {
                let path = error.instance_path.to_string();
                if path.is_empty() {
                    error.to_string()
                } else {
                    format!("{path}: {error}")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(OrbitError::Execution(format!(
            "plugin tool '{tool_name}' returned output that violates its output_schema: {details}"
        )));
    }
    Ok(())
}
