//! Admission, tool context, source checkout and sandbox preparation that run
//! before a provider subprocess is built.

use std::path::{Path, PathBuf};

use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;

use super::super::super::dispatcher::{DispatchError, ResolvedSandbox};
use super::super::super::workspace::resolve_subprocess_cwd;
use super::super::inspection::SourceInspection;
use super::super::spawn::{PreparedSandbox, prepare_sandbox_for_dispatch};
use super::super::supervisor::DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS;
use crate::context::RuntimeHost;

/// Trusted-host invocations have been removed. Keep refusing legacy assets
/// rather than allowing a stored admission to authorize a new process.
pub(super) fn reject_removed_trusted_host_execution(
    spec: &AgentLoopSpec,
    activity_name: &str,
) -> Result<(), DispatchError> {
    if spec.trusted_host_execution {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "activity `{activity_name}` requests removed trusted host execution; use a task workflow"
        )));
    }
    Ok(())
}

/// The provider's wall-clock bound in seconds from the resolved activity spec.
pub(super) fn invocation_timeout_seconds(spec: &AgentLoopSpec) -> u64 {
    if spec.wall_clock_timeout_seconds == 0 {
        DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS
    } else {
        spec.wall_clock_timeout_seconds
    }
}

/// Conflict recovery acts only for the failed target it was dispatched for.
pub(super) fn require_conflict_recovery_target(
    activity_name: &str,
    task_ids: &[String],
    input: &Value,
    run_id: &str,
) -> Result<(), DispatchError> {
    if activity_name == "pr_conflict_recovery"
        && (task_ids.is_empty() || input.get("run_id").and_then(Value::as_str) != Some(run_id))
    {
        return Err(DispatchError::CliInvocationPermanent(
            "conflict recovery requires task IDs and the current run ID from its failed target"
                .to_string(),
        ));
    }
    Ok(())
}

/// The activity's tool context, attributed to this provider and model.
pub(super) fn activity_tool_context(
    host: &dyn RuntimeHost,
    spec: &AgentLoopSpec,
    run_id: &str,
    fs_profile: Option<&str>,
    provider: &str,
) -> ToolContext {
    let mut tool_ctx = host.tool_context_for_activity(
        Some(run_id),
        fs_profile,
        None,
        spec.proc_allowed_programs.as_deref(),
    );
    tool_ctx.proc_disallowed_programs = spec.proc_disallowed_programs.clone();
    tool_ctx.agent_name = Some(provider.to_string());
    tool_ctx.model_name = spec.model.as_deref().map(str::to_string);
    tool_ctx
}

/// The checkout a provider runs in: the assigned source cwd, or the
/// temporary checkout of a source inspection with its rebound inputs.
pub(super) struct SourceCheckout {
    pub(super) source_cwd: Option<PathBuf>,
    pub(super) inspection: Option<SourceInspection>,
    pub(super) inspection_input: Option<Value>,
    pub(super) inspection_task_ctx: Option<Value>,
    pub(super) subprocess_cwd: Option<PathBuf>,
    pub(super) subprocess_cwd_string: Option<String>,
}

/// Resolve the subprocess cwd before sandbox compilation so the host can
/// re-allow the active worktree subpath after the policy deny rules. The
/// sandbox's `denyModify .orbit/**` rule otherwise blocks every non-codex
/// provider from writing inside its own jrun worktree. See T20260508-17.
pub(super) fn prepare_source_checkout(
    input: &Value,
    task_ctx: Option<&Value>,
    workspace_root: Option<&Path>,
    fs_profile: Option<&str>,
) -> Result<SourceCheckout, DispatchError> {
    let source_cwd = resolve_subprocess_cwd(input, task_ctx, workspace_root)?;
    let inspection = SourceInspection::from_input(input, source_cwd.as_deref(), fs_profile)?;
    let inspection_input = inspection
        .as_ref()
        .map(|snapshot| snapshot.bind_input(input));
    let inspection_task_ctx = inspection
        .as_ref()
        .and_then(|snapshot| task_ctx.map(|task| snapshot.bind_input(task)));
    let subprocess_cwd = inspection
        .as_ref()
        .map(|snapshot| snapshot.root().to_path_buf())
        .or_else(|| source_cwd.clone());
    let subprocess_cwd_string = subprocess_cwd
        .as_ref()
        .map(|path| path.display().to_string());
    Ok(SourceCheckout {
        source_cwd,
        inspection,
        inspection_input,
        inspection_task_ctx,
        subprocess_cwd,
        subprocess_cwd_string,
    })
}

/// Resolve the activity's ordinary executor sandbox.
pub(super) fn resolve_dispatch_sandbox(
    host: &dyn RuntimeHost,
    provider: &str,
    fs_profile: Option<&str>,
    subprocess_cwd: Option<&Path>,
) -> Result<Option<ResolvedSandbox>, DispatchError> {
    host.resolve_executor_sandbox(provider, fs_profile, subprocess_cwd)
}

/// Prepare the resolved sandbox for spawn under the executor's fallback policy.
pub(super) fn prepare_dispatch_sandbox<'a>(
    resolved_sandbox: Option<&'a ResolvedSandbox>,
) -> Result<PreparedSandbox<'a>, DispatchError> {
    prepare_sandbox_for_dispatch(resolved_sandbox)
        .map_err(|error| DispatchError::CliInvocationPermanent(error.message))
}
