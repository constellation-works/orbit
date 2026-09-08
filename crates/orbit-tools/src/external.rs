use std::env;

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode, run_process};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::builtin::proc::spawn::{ActivityFsSandbox, enforce_program_allowlist};
use crate::{TIMEOUT_DEFAULT_MS, Tool, ToolContext};

const EXTERNAL_TOOL_TIMEOUT_OVERRIDE_ENV: &str = "ORBIT_EXTERNAL_TOOL_TIMEOUT_MS";
const ORBIT_TOOL_NAME_ENV: &str = "ORBIT_TOOL_NAME";
const ORBIT_TOOL_CWD_ENV: &str = "ORBIT_TOOL_CWD";
const ORBIT_TOOL_WORKSPACE_ROOT_ENV: &str = "ORBIT_TOOL_WORKSPACE_ROOT";
const ORBIT_TOOL_AGENT_NAME_ENV: &str = "ORBIT_TOOL_AGENT_NAME";
const ORBIT_TOOL_MODEL_NAME_ENV: &str = "ORBIT_TOOL_MODEL_NAME";
const ORBIT_TOOL_ALLOWED_TOOLS_ENV: &str = "ORBIT_TOOL_ALLOWED_TOOLS";
const ORBIT_TOOL_PROC_ALLOWED_PROGRAMS_ENV: &str = "ORBIT_TOOL_PROC_ALLOWED_PROGRAMS";

pub struct ExternalTool {
    pub name: String,
    pub path: String,
    pub description: String,
    pub parameters: Vec<ToolParam>,
}

impl Tool for ExternalTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name.clone(),
            description: external_tool_description(&self.description),
            parameters: self.parameters.clone(),
            builtin: false,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        let input_json = serde_json::to_string(&input)
            .map_err(|e| OrbitError::Execution(format!("failed to serialize input: {e}")))?;
        let cwd = ctx.cwd.clone().ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "external tool '{}' requires ToolContext.cwd",
                self.name
            ))
        })?;
        let timeout_ms = external_tool_timeout_ms()?;
        enforce_program_allowlist(ctx, "external tool", &self.path)?;
        let environment_mode =
            EnvironmentMode::ClearAndSet(runtime_environment(ctx, &self.name, &cwd));

        let output = run_process(
            &ExecRequest {
                program: self.path.clone(),
                args: vec![],
                current_dir: Some(cwd),
                timeout_ms: Some(timeout_ms),
                stdin_mode: StdinMode::Bytes(input_json.into_bytes()),
                environment_mode,
                debug: false,
            },
            &ActivityFsSandbox::new(ctx)?,
        )?;

        if !output.success {
            return Err(OrbitError::Execution(format!(
                "tool '{}' exited with {}: {}",
                self.name,
                output.exit_code.unwrap_or(1),
                output.stderr.trim()
            )));
        }

        serde_json::from_str(output.stdout.trim()).map_err(|e| {
            OrbitError::Execution(format!(
                "tool '{}' produced invalid JSON output: {e}",
                self.name
            ))
        })
    }
}

fn external_tool_description(base: &str) -> String {
    let runtime_contract = format!(
        "Runtime contract: Orbit sends JSON input on stdin, expects JSON output on stdout, executes with cwd from ToolContext.cwd, exports ORBIT_TOOL_* context env vars, and enforces a default timeout of {} ms (override with {}).",
        TIMEOUT_DEFAULT_MS, EXTERNAL_TOOL_TIMEOUT_OVERRIDE_ENV
    );
    if base.trim().is_empty() {
        runtime_contract
    } else {
        format!("{base} {runtime_contract}")
    }
}

fn external_tool_timeout_ms() -> Result<u64, OrbitError> {
    let Some(raw) = env::var(EXTERNAL_TOOL_TIMEOUT_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(TIMEOUT_DEFAULT_MS);
    };

    raw.parse::<u64>().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid {} value '{}': {}",
            EXTERNAL_TOOL_TIMEOUT_OVERRIDE_ENV, raw, error
        ))
    })
}

fn runtime_environment(ctx: &ToolContext, tool_name: &str, cwd: &str) -> Vec<(String, String)> {
    let mut env_pairs = ctx
        .proc_spawn_environment
        .clone()
        .unwrap_or_else(|| allowlisted_child_env(&[], &[]));
    upsert_env(&mut env_pairs, ORBIT_TOOL_NAME_ENV, tool_name.to_string());
    upsert_env(&mut env_pairs, ORBIT_TOOL_CWD_ENV, cwd.to_string());
    if let Some(workspace_root) = ctx.workspace_root.as_ref() {
        upsert_env(
            &mut env_pairs,
            ORBIT_TOOL_WORKSPACE_ROOT_ENV,
            workspace_root.to_string_lossy().into_owned(),
        );
    }
    if let Some(agent_name) = ctx.agent_name.as_ref() {
        upsert_env(
            &mut env_pairs,
            ORBIT_TOOL_AGENT_NAME_ENV,
            agent_name.clone(),
        );
    }
    if let Some(model_name) = ctx.model_name.as_ref() {
        upsert_env(
            &mut env_pairs,
            ORBIT_TOOL_MODEL_NAME_ENV,
            model_name.clone(),
        );
    }
    if !ctx.allowed_tools.is_empty() {
        upsert_env(
            &mut env_pairs,
            ORBIT_TOOL_ALLOWED_TOOLS_ENV,
            ctx.allowed_tools.join(","),
        );
    }
    if !ctx.proc_allowed_programs.is_empty() {
        upsert_env(
            &mut env_pairs,
            ORBIT_TOOL_PROC_ALLOWED_PROGRAMS_ENV,
            ctx.proc_allowed_programs.join(","),
        );
    }
    env_pairs
}

fn upsert_env(env_pairs: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env_pairs.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env_pairs.push((key.to_string(), value));
    }
}
