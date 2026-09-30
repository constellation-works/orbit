//! The versioned call envelope (§4.2), the `context` both dispatch surfaces
//! carry, a backend's `secret_updates`, and `output_schema` validation.
//!
//! There is no partial success: anything short of `{"ok": true, "output":
//! <valid>}` is a tool error naming the cause, and the caller never sees
//! the backend's bytes. A secret rotation is not part of that answer: it is
//! applied or refused on its own, and never changes what the call returns.

use std::cell::RefCell;
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::plugin::{PluginSecretUpdateStatus, is_valid_secret_name};
use serde_json::{Value, json};

use super::backend::{DeliveredPluginSecret, PluginBackendSpec, PluginSecretRotation};
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

    /// `text` with every secret value this call carried replaced by a marker.
    ///
    /// For backend text the host relays to the caller, such as the stderr of a
    /// failed process. The backend was handed the values and can print them;
    /// the caller it reports to must not be able to read them there.
    pub(crate) fn mask_delivered(&self, text: &str) -> String {
        // Longest first, so a secret that contains another is masked whole
        // rather than leaving the rest of it readable.
        let mut values: Vec<&str> = self
            .0
            .iter()
            .flat_map(BTreeMap::values)
            .map(|secret| secret.value.as_str())
            .filter(|value| !value.is_empty())
            .collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let mut masked = text.to_string();
        for value in values {
            masked = masked.replace(value, "[secret]");
            // Validators and transport errors may quote JSON strings. Match
            // that representation too, without changing the JSON structure.
            if let Ok(quoted) = serde_json::to_string(value) {
                let escaped = &quoted[1..quoted.len() - 1];
                if escaped != value {
                    masked = masked.replace(escaped, "[secret]");
                }
            }
        }
        masked
    }

    /// Mask decoded JSON strings and object keys, retaining other value types.
    pub(crate) fn mask_json(&self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.mask_delivered(&text)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|item| self.mask_json(item)).collect())
            }
            Value::Object(fields) => {
                let mut masked = serde_json::Map::new();
                let mut renamed = Vec::new();
                for (key, value) in fields {
                    let public_key = self.mask_delivered(&key);
                    let value = self.mask_json(value);
                    if public_key == key {
                        masked.insert(key, value);
                    } else {
                        renamed.push((public_key, value));
                    }
                }
                // Preserve ordinary fields even when a secret key masks to
                // an existing key, and retain every renamed field's value.
                for (key, value) in renamed {
                    let mut public_key = key.clone();
                    let mut suffix = 2;
                    while masked.contains_key(&public_key) {
                        public_key = format!("{key} ({suffix})");
                        suffix += 1;
                    }
                    masked.insert(public_key, value);
                }
                Value::Object(masked)
            }
            other => other,
        }
    }

    /// Backend responses and schema/transport diagnostics leave through these
    /// variants. Other errors originate in host setup, before a reply is read.
    pub(crate) fn mask_error(&self, error: OrbitError) -> OrbitError {
        match error {
            OrbitError::Execution(message) => OrbitError::Execution(self.mask_delivered(&message)),
            OrbitError::RemoteTool {
                code,
                message,
                payload,
            } => OrbitError::RemoteTool {
                code: self.mask_delivered(&code),
                message: self.mask_delivered(&message),
                payload: self.mask_json(payload),
            },
            other => other,
        }
    }

    /// Record that this call's request is about to carry these secrets, so
    /// the audit row for the call can name them. Called once the request
    /// is built and immediately before it is sent.
    pub(crate) fn record_delivery(&self) {
        let names = self
            .0
            .as_ref()
            .map(|secrets| {
                secrets
                    .keys()
                    .map(|name| self.mask_delivered(name))
                    .collect()
            })
            .unwrap_or_default();
        DELIVERED_SECRET_NAMES.with(|cell| *cell.borrow_mut() = names);
    }
}

thread_local! {
    static DELIVERED_SECRET_NAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SECRET_UPDATES: RefCell<BTreeMap<String, PluginSecretUpdateStatus>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Take the names of the secrets the last plugin call on this thread
/// delivered, clearing the record. The audited dispatch boundary calls it
/// before a call (to drop anything stale) and after (to write the names on
/// the call's audit row). Names only: a value never leaves the request.
pub fn take_delivered_plugin_secret_names() -> Vec<String> {
    DELIVERED_SECRET_NAMES.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Take what the last plugin call on this thread did with its backend's
/// `secret_updates` — each name and whether it was applied or refused —
/// clearing the record. The audited dispatch boundary drains it around a call
/// the same way as [`take_delivered_plugin_secret_names`].
pub fn take_plugin_secret_updates() -> BTreeMap<String, PluginSecretUpdateStatus> {
    SECRET_UPDATES.with(|cell| std::mem::take(&mut *cell.borrow_mut()))
}

/// Why the host refused one `secret_updates` entry. The diagnostic built
/// from it names the secret and the cause, never a value.
enum SecretUpdateRefusal {
    Undeclared,
    NotRotatable,
    Malformed,
    Stale,
    Unsupported,
    Failed(OrbitError),
}

impl SecretUpdateRefusal {
    fn reason(&self) -> String {
        match self {
            Self::Undeclared => "the manifest does not declare it in spec.secrets".to_string(),
            Self::NotRotatable => "the manifest does not declare it `rotatable`".to_string(),
            Self::Malformed => "the entry is not `{\"value\": <string>, \"expected_version\": \
                                <string or null>}`"
                .to_string(),
            Self::Stale => "its expected_version is not the stored version; another update or \
                            `orbit plugin secret set` got there first. The next call carries \
                            the stored value and version"
                .to_string(),
            Self::Unsupported => "this run does not store secret updates".to_string(),
            Self::Failed(error) => format!("the store could not apply it: {error}"),
        }
    }
}

/// Apply a backend's `secret_updates` (design §3, "Plugin secrets"): `exec`
/// returns it beside `ok`/`output`, `mcp` as `result._meta.orbit.
/// secret_updates`. Each entry is `{"value": …, "expected_version": …}`,
/// where `expected_version` is the version the call was delivered (`null`:
/// only while the secret is unset).
///
/// Only a declared `rotatable` name is written, and only through the
/// source's compare-and-swap, so of two calls rotating from the same version
/// exactly one is applied. Every refusal is non-fatal: it is logged as a
/// diagnostic naming the secret and the cause — never a value — and the call
/// still returns whatever it would have returned. The outcomes are kept for
/// the call's audit row ([`take_plugin_secret_updates`]) and returned.
pub(crate) fn apply_secret_updates(
    spec: &PluginBackendSpec,
    tool_name: &str,
    secrets: &CallSecrets,
    updates: Option<&Value>,
) -> BTreeMap<String, PluginSecretUpdateStatus> {
    let mut outcomes = BTreeMap::new();
    let Some(updates) = updates.filter(|updates| !updates.is_null()) else {
        SECRET_UPDATES.with(|cell| cell.borrow_mut().clear());
        return outcomes;
    };
    let Some(entries) = updates.as_object() else {
        tracing::warn!(
            target: "orbit.tools.plugin",
            plugin = %spec.provenance.name,
            tool = %tool_name,
            "refused the backend's secret_updates: it is not an object of name to \
             {{value, expected_version}}; nothing was stored",
        );
        SECRET_UPDATES.with(|cell| cell.borrow_mut().clear());
        return outcomes;
    };
    for (name, entry) in entries {
        // The name is the backend's own bytes: one that is not a valid
        // secret name is neither logged nor audited, so a value put in a
        // name's place cannot reach either.
        if !is_valid_secret_name(name) {
            tracing::warn!(
                target: "orbit.tools.plugin",
                plugin = %spec.provenance.name,
                tool = %tool_name,
                "refused a secret update whose name is not a valid secret name",
            );
            continue;
        }
        let public_name = secrets.mask_delivered(name);
        let status = match apply_secret_update(spec, name, entry) {
            Ok(version) => {
                tracing::info!(
                    target: "orbit.tools.plugin",
                    plugin = %spec.provenance.name,
                    tool = %tool_name,
                    secret = %public_name,
                    version = %secrets.mask_delivered(&version),
                    "applied the backend's update to a rotatable secret",
                );
                PluginSecretUpdateStatus::Applied
            }
            Err(refusal) => {
                tracing::warn!(
                    target: "orbit.tools.plugin",
                    plugin = %spec.provenance.name,
                    tool = %tool_name,
                    secret = %public_name,
                    "refused the backend's update to secret '{public_name}': {}; the call's result is \
                     returned unchanged",
                    secrets.mask_delivered(&refusal.reason()),
                );
                PluginSecretUpdateStatus::Refused
            }
        };
        outcomes.insert(public_name, status);
    }
    SECRET_UPDATES.with(|cell| *cell.borrow_mut() = outcomes.clone());
    outcomes
}

/// One entry: the new version when applied, or why it was refused.
fn apply_secret_update(
    spec: &PluginBackendSpec,
    name: &str,
    entry: &Value,
) -> Result<String, SecretUpdateRefusal> {
    if !spec.secrets.declares(name) {
        return Err(SecretUpdateRefusal::Undeclared);
    }
    if !spec.secrets.is_rotatable(name) {
        return Err(SecretUpdateRefusal::NotRotatable);
    }
    let value = entry.get("value").and_then(Value::as_str);
    let expected_version = match entry.get("expected_version") {
        Some(Value::String(version)) => Some(Some(version.as_str())),
        Some(Value::Null) => Some(None),
        _ => None,
    };
    let (Some(value), Some(expected_version)) = (value, expected_version) else {
        return Err(SecretUpdateRefusal::Malformed);
    };
    match spec
        .secrets
        .compare_and_swap(name, value, expected_version)
        .map_err(SecretUpdateRefusal::Failed)?
    {
        PluginSecretRotation::Applied { version } => Ok(version),
        PluginSecretRotation::Stale => Err(SecretUpdateRefusal::Stale),
        PluginSecretRotation::Unsupported => Err(SecretUpdateRefusal::Unsupported),
    }
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
    response_output(tool_name, &parse_response_json(tool_name, stdout)?)
}

/// The backend's stdout as the JSON response it must be.
pub(crate) fn parse_response_json(tool_name: &str, stdout: &str) -> Result<Value, OrbitError> {
    serde_json::from_str(stdout.trim()).map_err(|error| {
        OrbitError::Execution(format!(
            "plugin tool '{tool_name}' produced invalid JSON output: {error}"
        ))
    })
}

/// A parsed response's `output`, or the error it reported.
pub(crate) fn response_output(tool_name: &str, response: &Value) -> Result<Value, OrbitError> {
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
