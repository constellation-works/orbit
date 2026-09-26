//! The versioned call envelope (§4.2), the `context` both dispatch surfaces
//! carry, and `output_schema` validation.
//!
//! There is no partial success: anything short of `{"ok": true, "output":
//! <valid>}` is a tool error naming the cause, and the caller never sees
//! the backend's bytes.

use std::cell::RefCell;
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::backend::{DeliveredPluginSecret, PluginBackendSpec};
use super::schema::CompiledSchema;
use crate::ToolContext;

/// The stdin envelope version the backend receives.
pub const PLUGIN_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Maximum serialized JSON bytes retained from a backend's error detail.
pub(crate) const MAX_PLUGIN_ERROR_DETAIL_BYTES: usize = 16 * 1024;

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
/// `task_id` and `job_run_id` name the managed activity the call serves. They
/// come from [`ToolContext::activity_binding`], which only the dispatching
/// host sets, so a backend can bind a write to the task that authorized it;
/// they are `null` for an interactive call. Tool input never reaches them.
///
/// `tool_name` is `Some` only for `mcp`: the `exec` envelope already names the
/// tool at its top level, while one `mcp` child serves every tool of its
/// plugin and has no `ORBIT_TOOL_NAME` in its environment (§4.2).
///
/// `secrets` is what [`CallSecrets::resolve`] read for this call: present
/// only when the plugin declares secrets, holding `{value, version}` for each
/// declared one that is set. This request is the only place a value travels —
/// never the child's environment or argv.
pub(crate) fn call_context(
    spec: &PluginBackendSpec,
    ctx: &ToolContext,
    tool_name: Option<&str>,
    secrets: &CallSecrets,
) -> Value {
    let mut context = json!({
        "workspace_root": ctx
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        "agent": ctx.agent_name,
        "model": ctx.model_name,
        "config": spec.config.as_value(),
        "task_id": ctx
            .activity_binding
            .as_ref()
            .and_then(|binding| binding.task_id.as_deref()),
        "job_run_id": ctx
            .activity_binding
            .as_ref()
            .map(|binding| binding.job_run_id.as_str()),
    });
    if let Some(fields) = context.as_object_mut() {
        if let Some(tool_name) = tool_name {
            fields.insert("tool".to_string(), Value::String(tool_name.to_string()));
        }
        if let Some(secrets) = &secrets.0 {
            let delivered = secrets
                .iter()
                .map(|(name, secret)| {
                    (
                        name.clone(),
                        json!({ "value": secret.value, "version": secret.version }),
                    )
                })
                .collect();
            fields.insert("secrets".to_string(), Value::Object(delivered));
        }
    }
    context
}

/// The declared secrets one call carries, read once for that call.
#[derive(Debug, Default)]
pub(crate) struct CallSecrets(Option<BTreeMap<String, DeliveredPluginSecret>>);

impl CallSecrets {
    /// Read the plugin's declared secrets for one call. A read that fails
    /// fails the call before anything is spawned or sent.
    pub(crate) fn resolve(spec: &PluginBackendSpec) -> Result<Self, OrbitError> {
        spec.secrets.resolve().map(Self)
    }

    /// Record that this call's request is about to carry these secrets, so
    /// the audit row for the call can name them. Called once the request
    /// is built and immediately before it is sent.
    pub(crate) fn record_delivery(&self) {
        let names = self
            .0
            .as_ref()
            .map(|secrets| secrets.keys().cloned().collect())
            .unwrap_or_default();
        DELIVERED_SECRET_NAMES.with(|cell| *cell.borrow_mut() = names);
    }
}

thread_local! {
    static DELIVERED_SECRET_NAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Take the names of the secrets the last plugin call on this thread
/// delivered, clearing the record. The audited dispatch boundary calls it
/// before a call (to drop anything stale) and after (to write the names on
/// the call's audit row). Names only: a value never leaves the request.
pub fn take_delivered_plugin_secret_names() -> Vec<String> {
    DELIVERED_SECRET_NAMES.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// The whole stdin envelope one `exec` call writes to its backend.
pub(crate) fn exec_envelope(
    spec: &PluginBackendSpec,
    ctx: &ToolContext,
    tool_name: &str,
    input: Value,
    secrets: &CallSecrets,
) -> Value {
    json!({
        "schema_version": PLUGIN_ENVELOPE_SCHEMA_VERSION,
        "tool": tool_name,
        "input": input,
        "context": call_context(spec, ctx, None, secrets),
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
            if let Some(error) = plugin_error(tool_name, &error) {
                return Err(error);
            }
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

/// Accept only a well-formed backend error. Keep its public payload separate
/// from the backend's untrusted object, so unknown fields cannot leak out.
pub(crate) fn plugin_error(tool_name: &str, error: &Value) -> Option<OrbitError> {
    let code = error.get("code")?.as_str()?;
    let message = error.get("message")?.as_str()?;
    if code.trim().is_empty() || message.trim().is_empty() {
        return None;
    }
    let retryable = match error.get("retryable") {
        Some(value) => value.as_bool()?,
        None => false,
    };
    let mut payload = json!({
        "code": code,
        "message": message,
        "retryable": retryable,
    });
    if let Some(detail) = error.get("detail")
        && serde_json::to_vec(detail).ok()?.len() <= MAX_PLUGIN_ERROR_DETAIL_BYTES
    {
        payload["detail"] = detail.clone();
    }
    Some(OrbitError::RemoteTool {
        code: code.to_string(),
        message: format!("plugin tool '{tool_name}' failed: {message}"),
        payload,
    })
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
