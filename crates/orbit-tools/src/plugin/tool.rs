//! A plugin tool: one `spec.tools[]` entry bound to its plugin's backend.
//!
//! `exec` runs one confined process per call with the versioned JSON
//! envelope on stdin and `{"ok": …}` on stdout; `mcp` proxies the call to
//! the plugin's long-lived stdio server for that caller's allowed-tools
//! intersection (§4.2). Both validate the output against `output_schema`
//! before the caller sees it.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode, supervise_child};
use orbit_types::plugin::{PluginExecutionKind, PluginProvenance};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use super::backend::PluginBackendSpec;
use super::callback::PluginCallbackSession;
use super::envelope::{PLUGIN_ENVELOPE_SCHEMA_VERSION, parse_response, validate_output};
use super::mcp::McpBackend;
use crate::{Tool, ToolContext, ToolExecutionKind};

/// What the registry knows about a plugin-backed entry beyond its schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolBinding {
    pub provenance: PluginProvenance,
    pub execution_kind: PluginExecutionKind,
    /// Set on an inactive entry: why the plugin is not on the active surface
    /// and what would fix it.
    pub diagnostic: Option<String>,
}

/// How a plugin's tools reach their backend.
#[derive(Clone)]
pub enum PluginBackend {
    /// One process per call.
    Exec(Arc<PluginBackendSpec>),
    /// One stdio MCP server per allowed-tools intersection per runtime,
    /// shared by every tool of the plugin.
    Mcp(Arc<McpBackend>),
}

impl PluginBackend {
    pub fn spec(&self) -> &Arc<PluginBackendSpec> {
        match self {
            Self::Exec(spec) => spec,
            Self::Mcp(backend) => backend.spec(),
        }
    }
}

pub struct PluginTool {
    /// Canonical `<ns>.<verb>` (or `orbit.<ns>.<verb>`).
    pub name: String,
    /// The manifest verb, which is also the `mcp` server's tool name.
    pub verb: String,
    pub description: String,
    pub parameters: Vec<ToolParam>,
    pub execution_kind: PluginExecutionKind,
    pub output_schema: Option<Value>,
    pub binding: Arc<PluginToolBinding>,
    pub backend: PluginBackend,
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
        // The backend program itself is not subject to the caller's program
        // allowlist: it is a fixed program from a manifest the operator
        // installed and enabled, gated by the activity's `allowed_tools` and
        // the governed-operation row before this point. What the backend
        // declares it *spawns* (`requires.programs`) is bounded by that
        // allowlist, through the same gate `proc.spawn` applies.
        self.backend.spec().enforce_programs(ctx, &self.name)?;
        let output = match &self.backend {
            PluginBackend::Exec(spec) => self.execute_process(spec, ctx, input)?,
            PluginBackend::Mcp(backend) => backend.call(ctx, &self.name, &self.verb, input)?,
        };
        validate_output(&self.name, self.output_schema.as_ref(), &output)?;
        Ok(output)
    }
}

impl PluginTool {
    fn execute_process(
        &self,
        spec: &PluginBackendSpec,
        ctx: &ToolContext,
        input: Value,
    ) -> Result<Value, OrbitError> {
        let cwd = ctx.cwd.clone().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin tool '{}' requires ToolContext.cwd",
                self.name
            ))
        })?;
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
        let timeout_ms = spec.timeout_ms();
        let sandbox = spec.sandbox_profile(ctx.workspace_root.as_deref())?;
        let mut environment = spec.child_environment(ctx, &cwd, Some(&self.name));
        let mut callback = PluginCallbackSession::mint(&spec.global_root, &spec.provenance)?;
        callback.stamp_env(&mut environment);
        let request = ExecRequest {
            program: spec.command.to_string_lossy().into_owned(),
            args: spec.args.clone(),
            current_dir: Some(cwd.clone()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Bytes(stdin.clone()),
            environment_mode: EnvironmentMode::ClearAndSet(environment),
            debug: false,
        };
        sandbox.validate(&request)?;
        let mut child = sandbox.spawn(&request)?;
        if let Err(error) = callback.bind_pid(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let output = supervise_child(child, Some(timeout_ms), Some(stdin))?.result;

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
        parse_response(&self.name, &output.stdout)
    }
}
