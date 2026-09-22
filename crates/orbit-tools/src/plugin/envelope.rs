//! The versioned call envelope (§4.2), the `context` both dispatch surfaces
//! carry, and `output_schema` validation.
//!
//! There is no partial success: anything short of `{"ok": true, "output":
//! <valid>}` is a tool error naming the cause, and the caller never sees
//! the backend's bytes.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::backend::PluginBackendSpec;
use super::schema::CompiledSchema;
use crate::ToolContext;

/// The stdin envelope version the backend receives.
pub const PLUGIN_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// What a backend is told about the call it is serving.
///
/// `exec` sends it as the envelope's `context` on stdin and `mcp` sends it as
/// `params._meta.orbit` on `tools/call` (`mcp.rs`), which is why it is built
/// here: one resolution, so the two dispatch surfaces cannot tell the same
/// plugin two different things about the same workspace. `config` is the
/// effective `[plugins.<ns>]` section the host validated against the plugin's
/// schema — the backend's own view of its configuration, which until now it
/// could only see through the manifest's `{{config.<key>}}` templates.
///
/// `tool_name` is `Some` only for `mcp`: the `exec` envelope already names the
/// tool at its top level, while one `mcp` child serves every tool of its
/// plugin and has no `ORBIT_TOOL_NAME` in its environment (§4.2).
pub(crate) fn call_context(
    spec: &PluginBackendSpec,
    ctx: &ToolContext,
    tool_name: Option<&str>,
) -> Value {
    let mut context = json!({
        "workspace_root": ctx
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        "agent": ctx.agent_name,
        "model": ctx.model_name,
        "config": spec.config.as_value(),
    });
    if let Some(tool_name) = tool_name
        && let Some(fields) = context.as_object_mut()
    {
        fields.insert("tool".to_string(), Value::String(tool_name.to_string()));
    }
    context
}

/// The whole stdin envelope one `exec` call writes to its backend.
pub(crate) fn exec_envelope(
    spec: &PluginBackendSpec,
    ctx: &ToolContext,
    tool_name: &str,
    input: Value,
) -> Value {
    json!({
        "schema_version": PLUGIN_ENVELOPE_SCHEMA_VERSION,
        "tool": tool_name,
        "input": input,
        "context": call_context(spec, ctx, None),
    })
}

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
