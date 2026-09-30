use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_common::tracing;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use super::git_config::enforce_no_persistent_git_config;
use crate::{TIMEOUT_DEFAULT_MS, TIMEOUT_LONG_MS, Tool, ToolContext};

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
                    description: format!(
                        "Execution timeout in milliseconds (default {TIMEOUT_DEFAULT_MS}; larger values are clamped to {MAX_TIMEOUT_MS})"
                    ),
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

        enforce_program_allowlist(ctx, "proc.spawn", &program)?;

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

        enforce_no_persistent_git_config("proc.spawn", &program, &args)?;

        let request = spawn_request(ctx, program, args, proc_spawn_timeout_ms(&input));
        // A managed CLI worker already runs under Bubblewrap (Linux) or
        // sandbox-exec (macOS). Its children inherit that OS boundary. Adding
        // Landlock here would narrow reads again and refuse macOS outright.
        let exec_result = run_process(&request, &NoSandbox)?;

        serde_json::to_value(exec_result)
            .map_err(|e| OrbitError::Execution(format!("serialize exec result: {e}")))
    }
}

/// Assemble the child's execution request.
///
/// Two properties of this request are the tool's contract rather than caller
/// choices: the child environment and its stdin.
///
/// The runtime resolves `[execution.env]` once and hands the complete child
/// environment to this authoritative spawn boundary. Contexts without a
/// configuration layer receive Orbit's credential-free baseline rather than
/// falling back to ambient inheritance.
///
/// Stdin is closed, matching the engine's `local_shell` step and the other
/// process-spawning builtins. A `proc.spawn` child is non-interactive: with an
/// inherited stdin, a program that reads it (`cat`, a confirmation prompt) would
/// only ever end at the deadline, and until then it would consume the
/// operator's terminal keystrokes.
fn spawn_request(
    ctx: &ToolContext,
    program: String,
    args: Vec<String>,
    timeout_ms: u64,
) -> ExecRequest {
    let env_pairs = ctx
        .proc_spawn_environment
        .clone()
        .unwrap_or_else(|| allowlisted_child_env(&[], &[]));

    ExecRequest {
        program,
        args,
        current_dir: ctx
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        timeout_ms: Some(timeout_ms),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(env_pairs),
        debug: false,
    }
}

/// Ceiling for a caller-supplied `timeout_ms`.
///
/// The supervisor deadline is the only thing that ends a child that never
/// exits on its own, and `proc.spawn` reads that deadline straight from tool
/// input — so an asset asking for `u64::MAX` would otherwise disable it. The
/// ceiling is the longest timeout this crate defines, well above the read-only
/// commands the shipped activities grant `proc.spawn`.
const MAX_TIMEOUT_MS: u64 = TIMEOUT_LONG_MS;

fn proc_spawn_timeout_ms(input: &Value) -> u64 {
    let Some(requested) = input.get("timeout_ms").and_then(Value::as_u64) else {
        return TIMEOUT_DEFAULT_MS;
    };
    if requested > MAX_TIMEOUT_MS {
        tracing::warn!(
            target: "orbit.tool.proc_spawn",
            requested_timeout_ms = requested,
            timeout_ms = MAX_TIMEOUT_MS,
            "proc.spawn timeout exceeds the supervisor maximum and was clamped",
        );
        return MAX_TIMEOUT_MS;
    }
    requested
}

pub(crate) fn enforce_program_allowlist(
    ctx: &ToolContext,
    tool_name: &str,
    program: &str,
) -> Result<(), OrbitError> {
    if let Some(disallowed) = &ctx.proc_disallowed_programs {
        // Check the requested basename and the physical target. This also
        // catches an absolute-path spelling or symlink to a listed program.
        let requested = Path::new(program);
        let basename = requested.file_name().and_then(|name| name.to_str());
        let resolved = if requested.components().count() > 1 {
            std::fs::canonicalize(requested).ok()
        } else {
            let child_path = ctx.proc_spawn_environment.as_deref().and_then(|pairs| {
                pairs
                    .iter()
                    .find(|(name, _)| name == "PATH")
                    .map(|(_, value)| value.as_str())
            });
            let path = child_path
                .map(std::ffi::OsString::from)
                .or_else(|| std::env::var_os("PATH"));
            path.and_then(|path| {
                std::env::split_paths(&path)
                    .find_map(|dir| std::fs::canonicalize(dir.join(requested)).ok())
            })
        };
        let resolved_basename = resolved
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        if disallowed.iter().any(|entry| {
            entry == program
                || Some(entry.as_str()) == basename
                || resolved.as_deref() == Some(Path::new(entry))
                || resolved.as_deref() == std::fs::canonicalize(entry).ok().as_deref()
                || Some(entry.as_str()) == resolved_basename
        }) {
            tracing::warn!(
                target: "orbit.policy.deny",
                tool = tool_name,
                path = program,
                profile = "proc.disallowed_programs",
                matched_rule = disallowed.join(", ").as_str(),
            );
            return Err(OrbitError::PolicyDenied(format!(
                "program '{}' is in the activity disallow list: [{}]",
                program,
                disallowed.join(", ")
            )));
        }
        return Ok(());
    }
    // Enforce program allowlist when the call sits inside an activity-scoped
    // tool context, or when a legacy unrestricted context still has a
    // non-empty list. An activity-scoped call with an empty list denies every
    // program (fail-closed).
    let restricted = ctx.proc_spawn_activity_scoped || !ctx.proc_allowed_programs.is_empty();
    if restricted && !ctx.proc_allowed_programs.iter().any(|p| p == program) {
        let matched_rule = if ctx.proc_allowed_programs.is_empty() {
            "<no allowed programs>".to_string()
        } else {
            ctx.proc_allowed_programs.join(", ")
        };
        tracing::warn!(
            target: "orbit.policy.deny",
            tool = tool_name,
            path = program,
            profile = "proc.allowed_programs",
            matched_rule = matched_rule.as_str(),
        );
        return Err(OrbitError::PolicyDenied(format!(
            "program '{}' is not in the allowed list: [{}]",
            program, matched_rule
        )));
    }

    Ok(())
}

#[cfg(test)]
#[path = "tests/spawn.rs"]
mod tests;
