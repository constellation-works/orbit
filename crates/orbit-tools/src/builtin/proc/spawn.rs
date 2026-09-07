use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_common::tracing;
use orbit_exec::{
    EnvironmentMode, ExecRequest, Sandbox, StdinMode, run_process, spawn_under_linux_landlock,
};
use orbit_types::policy::FsOperation;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{TIMEOUT_DEFAULT_MS, Tool, ToolContext};

pub struct ProcSpawnTool;

impl Tool for ProcSpawnTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "proc.spawn".to_string(),
            description: "Spawn a process with timeout and capture output".to_string(),
            parameters: vec![
                ToolParam {
                    name: "program".to_string(),
                    description: "Program to execute".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                },
                ToolParam {
                    name: "args".to_string(),
                    description: "Arguments to pass to the program".to_string(),
                    param_type: "array".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "timeout_ms".to_string(),
                    description: "Execution timeout in milliseconds".to_string(),
                    param_type: "u64".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        let program = input
            .get("program")
            .and_then(Value::as_str)
            .ok_or_else(|| OrbitError::InvalidInput("missing `program`".to_string()))?
            .to_string();

        // Enforce program allowlist when the call sits inside an activity-scoped
        // tool context, or when a legacy unrestricted context still has a
        // non-empty list. An activity-scoped call with an empty list denies
        // every program (fail-closed).
        let restricted = ctx.proc_spawn_activity_scoped || !ctx.proc_allowed_programs.is_empty();
        if restricted && !ctx.proc_allowed_programs.iter().any(|p| p == &program) {
            let matched_rule = if ctx.proc_allowed_programs.is_empty() {
                "<no allowed programs>".to_string()
            } else {
                ctx.proc_allowed_programs.join(", ")
            };
            tracing::warn!(
                target: "orbit.policy.deny",
                tool = "proc.spawn",
                path = program.as_str(),
                profile = "proc.allowed_programs",
                matched_rule = matched_rule.as_str(),
            );
            return Err(OrbitError::PolicyDenied(format!(
                "program '{}' is not in the allowed list: [{}]",
                program, matched_rule
            )));
        }

        let args = input
            .get("args")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let timeout_ms = proc_spawn_timeout_ms(&input);

        // The runtime resolves `[execution.env]` once and hands the complete
        // child environment to this authoritative spawn boundary. Contexts
        // without a configuration layer receive Orbit's credential-free
        // baseline rather than falling back to ambient inheritance.
        let env_pairs = ctx
            .proc_spawn_environment
            .clone()
            .unwrap_or_else(|| allowlisted_child_env(&[], &[]));

        let current_dir = ctx
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());

        let request = ExecRequest {
            program,
            args,
            current_dir,
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Inherit,
            environment_mode: EnvironmentMode::ClearAndSet(env_pairs),
            debug: false,
        };
        let sandbox = ActivityFsSandbox { ctx };
        let exec_result = run_process(&request, &sandbox)?;

        serde_json::to_value(exec_result)
            .map_err(|e| OrbitError::Execution(format!("serialize exec result: {e}")))
    }
}

/// Request-time filesystem gate plus process-boundary confinement for
/// activity-scoped subprocesses.
///
/// Existing path arguments (including `--key=path`) are still resolved
/// symlink-safely before spawn so an explicit `git -C /etc` stays a
/// `policy_denied`. The child's own interpreter — git aliases, `rg --pre`,
/// and any descendant — is confined by the Landlock read profile applied at
/// spawn, not by guessing which argv strings look like paths.
struct ActivityFsSandbox<'a> {
    ctx: &'a ToolContext,
}

impl Sandbox for ActivityFsSandbox<'_> {
    fn spawn(&self, request: &ExecRequest) -> Result<Child, OrbitError> {
        if !self.ctx.proc_spawn_activity_scoped {
            return orbit_exec::process::spawn(request);
        }
        let (Some(policy), Some(profile), Some(workspace_root)) = (
            self.ctx.policy_engine.as_ref(),
            self.ctx.fs_profile.as_deref(),
            self.ctx.workspace_root.as_deref(),
        ) else {
            return Err(OrbitError::PolicyDenied(
                "activity-scoped proc.spawn is missing its resolved filesystem policy".to_string(),
            ));
        };
        let resolved = policy.def().effective_profile(profile)?;
        spawn_under_linux_landlock(request, workspace_root, &resolved)
    }

    fn validate(&self, request: &ExecRequest) -> Result<(), OrbitError> {
        if !self.ctx.proc_spawn_activity_scoped {
            return Ok(());
        }
        let (Some(policy), Some(profile), Some(workspace_root)) = (
            self.ctx.policy_engine.as_ref(),
            self.ctx.fs_profile.as_deref(),
            self.ctx.workspace_root.as_deref(),
        ) else {
            return Err(OrbitError::PolicyDenied(
                "activity-scoped proc.spawn is missing its resolved filesystem policy".to_string(),
            ));
        };
        let cwd = request
            .current_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace_root.to_path_buf());
        for path in request
            .args
            .iter()
            .filter_map(|arg| path_argument(arg, &cwd))
        {
            let evaluation =
                policy.check_resolved(workspace_root, profile, FsOperation::Read, &path)?;
            if evaluation.allowed {
                continue;
            }
            tracing::warn!(
                target: "orbit.policy.deny",
                tool = "proc.spawn",
                path = evaluation.path.as_str(),
                profile = evaluation.profile.as_str(),
                matched_rule = evaluation.matched_rule.as_str(),
            );
            return Err(OrbitError::PolicyDenied(format!(
                "proc.spawn path '{}' is denied by fsProfile '{}' (matched rule: {})",
                evaluation.path, evaluation.profile, evaluation.matched_rule
            )));
        }
        Ok(())
    }
}

fn path_argument(argument: &str, cwd: &Path) -> Option<PathBuf> {
    let candidate = argument
        .strip_prefix('-')
        .and_then(|option| option.split_once('=').map(|(_, value)| value))
        .unwrap_or(argument);
    if candidate.is_empty() || candidate == "-" {
        return None;
    }
    let path = Path::new(candidate);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    (path.is_absolute()
        || candidate.starts_with('.')
        || resolved.exists()
        || resolved.symlink_metadata().is_ok())
    .then_some(resolved)
}

fn proc_spawn_timeout_ms(input: &Value) -> u64 {
    input
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(TIMEOUT_DEFAULT_MS)
}

#[cfg(test)]
#[path = "tests/spawn.rs"]
mod tests;
