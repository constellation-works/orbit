use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_common::tracing;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, Sandbox, StdinMode, run_process,
    spawn_under_linux_landlock,
};
use orbit_policy::PolicyEngine;
use orbit_types::policy::{FsOperation, ResolvedFsProfile};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

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

        let request = spawn_request(ctx, program, args, proc_spawn_timeout_ms(&input));
        let exec_result = run_process(&request, &ActivityFsSandbox::new(ctx)?)?;

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

/// Filesystem confinement for activity-scoped `proc.spawn`.
///
/// This type is the `proc.spawn` sandbox, not a generic activity subprocess
/// wrapper. Registered external tools share the program allowlist but stay on
/// the unconfined `NoSandbox` path; see `crates/orbit-tools/src/external.rs`.
///
/// Two layers, doing two different jobs. Explicit path arguments (including
/// `--key=path`) are resolved symlink-safely by the same policy engine the
/// filesystem tools use, so `git -C /etc` is refused before the child exists
/// and the caller is told which rule refused it. That check cannot be the
/// boundary, though: `bash`, `python3`, and `git` shell aliases all reach the
/// filesystem from text that never looks like a path argument. The read
/// boundary is therefore the ruleset applied to the child itself at spawn.
///
/// Outside an activity-scoped context there is no resolved profile to enforce,
/// and `proc.spawn` keeps its unconfined behavior with the program allowlist as
/// the only gate.
pub(crate) struct ActivityFsSandbox<'a> {
    scope: Option<ActivityScope<'a>>,
}

/// The resolved policy an activity-scoped child is confined to, read once so
/// the request-time check and the enforced ruleset cannot disagree.
struct ActivityScope<'a> {
    policy: &'a PolicyEngine,
    workspace_root: &'a Path,
    profile_name: &'a str,
    profile: ResolvedFsProfile,
}

impl<'a> ActivityFsSandbox<'a> {
    /// Fails closed: an activity-scoped context without a resolved filesystem
    /// policy cannot spawn anything.
    pub(crate) fn new(ctx: &'a ToolContext) -> Result<Self, OrbitError> {
        if !ctx.proc_spawn_activity_scoped {
            return Ok(Self { scope: None });
        }
        let (Some(policy), Some(profile_name), Some(workspace_root)) = (
            ctx.policy_engine.as_deref(),
            ctx.fs_profile.as_deref(),
            ctx.workspace_root.as_deref(),
        ) else {
            return Err(OrbitError::PolicyDenied(
                "activity-scoped proc.spawn is missing its resolved filesystem policy".to_string(),
            ));
        };
        let profile = policy.def().effective_profile(profile_name)?;
        Ok(Self {
            scope: Some(ActivityScope {
                policy,
                workspace_root,
                profile_name,
                profile,
            }),
        })
    }
}

impl Sandbox for ActivityFsSandbox<'_> {
    fn validate(&self, request: &ExecRequest) -> Result<(), OrbitError> {
        let Some(scope) = &self.scope else {
            return Ok(());
        };
        let cwd = request
            .current_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| scope.workspace_root.to_path_buf());
        for path in request
            .args
            .iter()
            .filter_map(|arg| path_argument(arg, &cwd))
        {
            let evaluation = scope.policy.check_resolved(
                scope.workspace_root,
                scope.profile_name,
                FsOperation::Read,
                &path,
            )?;
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

    fn spawn(&self, request: &ExecRequest) -> Result<Child, OrbitError> {
        match &self.scope {
            Some(scope) => {
                spawn_under_linux_landlock(request, scope.workspace_root, &scope.profile)
            }
            None => NoSandbox.spawn(request),
        }
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
