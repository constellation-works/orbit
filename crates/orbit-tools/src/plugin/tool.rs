//! A plugin tool backed by the manifest's `exec` backend: one process per
//! call, a versioned JSON envelope on stdin, `{"ok": …}` on stdout (§4.2).

use std::path::PathBuf;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::plugin::{PLUGIN_HOST_API, PluginExecutionKind, PluginProvenance};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{TIMEOUT_SLOW_MS, Tool, ToolContext, ToolExecutionKind};

/// The stdin envelope version the backend receives.
pub const PLUGIN_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Host ceiling on `spec.backend.timeout_ms`.
const PLUGIN_TIMEOUT_CEILING_MS: u64 = 300_000;

/// What the registry knows about a plugin-backed entry beyond its schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolBinding {
    pub provenance: PluginProvenance,
    pub execution_kind: PluginExecutionKind,
    /// Set on an inactive entry: why the plugin is not on the active surface
    /// and what would fix it.
    pub diagnostic: Option<String>,
}

pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub parameters: Vec<ToolParam>,
    pub execution_kind: PluginExecutionKind,
    pub binding: Arc<PluginToolBinding>,
    pub plugin_root: PathBuf,
    pub state_dir: PathBuf,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
    /// Requested `permissions.orbit_tools`; stamped into `ORBIT_ALLOWED_TOOLS`
    /// intersected with the caller's own allowlist when one is set.
    pub requested_orbit_tools: Vec<String>,
}

impl Tool for PluginTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            builtin: false,
        }
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        match self.execution_kind {
            PluginExecutionKind::ReadOnly => ToolExecutionKind::ReadOnly,
            PluginExecutionKind::Mutating => ToolExecutionKind::Mutating,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        let cwd = ctx.cwd.clone().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin tool '{}' requires ToolContext.cwd",
                self.name
            ))
        })?;
        // Deliberately not subject to the `proc.spawn` program allowlist. That
        // list governs a *caller-chosen* argv (`proc.spawn`) or a path handed
        // to `orbit tool add`; a plugin backend is a fixed program declared in
        // a manifest the operator installed and enabled, the way a builtin
        // spawns its own `git` or `gh`. An activity's real gate on this tool is
        // its `allowed_tools` list plus the governed-operation row for the
        // tool's execution kind, both applied before this point. Confinement of
        // the backend itself is the phase-2 sandbox (design §4.3).
        let program = self.command.to_string_lossy().into_owned();

        let envelope = json!({
            "schema_version": PLUGIN_ENVELOPE_SCHEMA_VERSION,
            "tool": self.name,
            "input": input,
            "context": {
                "workspace_root": ctx.workspace_root.as_ref().map(|path| path.to_string_lossy().into_owned()),
                "agent": ctx.agent_name,
                "model": ctx.model_name,
            },
        });
        let stdin = serde_json::to_vec(&envelope).map_err(|error| {
            OrbitError::Execution(format!("serialize plugin envelope: {error}"))
        })?;
        let timeout_ms = self
            .timeout_ms
            .unwrap_or(TIMEOUT_SLOW_MS)
            .min(PLUGIN_TIMEOUT_CEILING_MS);

        // Plugin backends are unconfined in this phase, exactly like external
        // tools: the program allowlist above is the gate, and sandboxing
        // arrives with the grant machinery (design §4.3).
        let output = run_process(
            &ExecRequest {
                program,
                args: self.args.clone(),
                current_dir: Some(cwd.clone()),
                timeout_ms: Some(timeout_ms),
                stdin_mode: StdinMode::Bytes(stdin),
                environment_mode: EnvironmentMode::ClearAndSet(self.runtime_environment(ctx, &cwd)),
                debug: false,
            },
            &NoSandbox,
        )?;

        if output.timed_out {
            return Err(OrbitError::Execution(format!(
                "plugin tool '{}' timed out after {timeout_ms} ms",
                self.name
            )));
        }
        if !output.success {
            return Err(OrbitError::Execution(format!(
                "plugin tool '{}' exited with {}: {}",
                self.name,
                output.exit_code.unwrap_or(1),
                output.stderr.trim()
            )));
        }
        let response: Value = serde_json::from_str(output.stdout.trim()).map_err(|error| {
            OrbitError::Execution(format!(
                "plugin tool '{}' produced invalid JSON output: {error}",
                self.name
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
                    "plugin tool '{}' failed ({code}): {message}",
                    self.name
                )))
            }
            None => Err(OrbitError::Execution(format!(
                "plugin tool '{}' returned an envelope without a boolean `ok`",
                self.name
            ))),
        }
    }
}

impl PluginTool {
    fn runtime_environment(&self, ctx: &ToolContext, cwd: &str) -> Vec<(String, String)> {
        let mut env_pairs = ctx
            .proc_spawn_environment
            .clone()
            .unwrap_or_else(|| allowlisted_child_env(&[], &[]));
        let allowed_tools = if ctx.allowed_tools.is_empty() {
            self.requested_orbit_tools.clone()
        } else {
            self.requested_orbit_tools
                .iter()
                .filter(|tool| ctx.allowed_tools.iter().any(|allowed| allowed == *tool))
                .cloned()
                .collect()
        };
        let mut set = |key: &str, value: String| upsert_env(&mut env_pairs, key, value);
        set("ORBIT_HOST_API", PLUGIN_HOST_API.to_string());
        set("ORBIT_VERSION", env!("CARGO_PKG_VERSION").to_string());
        set("ORBIT_PLUGIN", self.binding.provenance.name.clone());
        set(
            "ORBIT_PLUGIN_VERSION",
            self.binding.provenance.version.clone(),
        );
        set(
            "ORBIT_PLUGIN_ROOT",
            self.plugin_root.to_string_lossy().into_owned(),
        );
        set(
            "ORBIT_PLUGIN_STATE",
            self.state_dir.to_string_lossy().into_owned(),
        );
        set("ORBIT_TOOL_NAME", self.name.clone());
        set("ORBIT_TOOL_CWD", cwd.to_string());
        if let Some(workspace_root) = ctx.workspace_root.as_ref() {
            set(
                "ORBIT_WORKSPACE_ROOT",
                workspace_root.to_string_lossy().into_owned(),
            );
        }
        if !allowed_tools.is_empty() {
            set("ORBIT_ALLOWED_TOOLS", allowed_tools.join(","));
        }
        env_pairs
    }
}

fn upsert_env(env_pairs: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env_pairs.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env_pairs.push((key.to_string(), value));
    }
}
