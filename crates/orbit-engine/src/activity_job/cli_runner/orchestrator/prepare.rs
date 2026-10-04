//! Admission, tool context, source checkout and sandbox preparation that run
//! before a provider subprocess is built.

use std::path::{Path, PathBuf};

use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::{AgentLoopSpec, TrustedHostAdmission};
use serde_json::Value;

use super::super::super::dispatcher::{DispatchError, ResolvedSandbox};
use super::super::super::workspace::resolve_subprocess_cwd;
use super::super::inspection::SourceInspection;
use super::super::spawn::{PreparedSandbox, prepare_sandbox_for_dispatch};
use super::super::supervisor::DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS;
use crate::context::RuntimeHost;

/// [ORB-11354] Trusted host execution needs both halves: the built-in
/// activity declares the mode, and the operator's canonical submission
/// stamps the admission into the run input. A declaration without an
/// admission is a broken admission path, not a request for a sandboxed run,
/// so it fails closed here rather than silently downgrading.
pub(super) fn trusted_host_admission(
    spec: &AgentLoopSpec,
    activity_name: &str,
    input: &Value,
) -> Result<Option<TrustedHostAdmission>, DispatchError> {
    Ok(if spec.trusted_host_execution {
        Some(TrustedHostAdmission::from_run_input(input).ok_or_else(|| {
            DispatchError::CliInvocationPermanent(format!(
                "activity `{activity_name}` declares trusted host execution but this run carries \
                 no operator admission; submit it through the governed `orbit.agent.invoke` \
                 operation"
            ))
        })?)
    } else {
        None
    })
}

/// The provider's wall-clock bound in seconds: the activity's declared
/// timeout, only ever shortened by the run input.
pub(super) fn invocation_timeout_seconds(
    spec: &AgentLoopSpec,
    trusted_host: Option<&TrustedHostAdmission>,
    input: &Value,
) -> u64 {
    let declared_timeout_seconds = if spec.wall_clock_timeout_seconds == 0 {
        DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS
    } else {
        spec.wall_clock_timeout_seconds
    };
    // [ORB-11354] An operator submitting an exploration says how long they are
    // willing to wait. The request can only *shorten* the activity's declared
    // bound, so the asset stays the ceiling and no run input can extend an
    // unsandboxed subprocess past it. Only the admitted mode reads the key;
    // every other activity keeps its declared timeout verbatim.
    match trusted_host.and(
        input
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .filter(|seconds| *seconds > 0),
    ) {
        Some(requested) => requested.min(declared_timeout_seconds),
        None => declared_timeout_seconds,
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

/// An admitted trusted-host invocation skips sandbox resolution entirely
/// rather than resolving one and discarding it: there is no profile to
/// compile, no wrapper to probe, and no inner-sandbox flag to neutralize.
/// Every other activity, including every managed task job, is unchanged.
pub(super) fn resolve_dispatch_sandbox(
    host: &dyn RuntimeHost,
    provider: &str,
    fs_profile: Option<&str>,
    subprocess_cwd: Option<&Path>,
    trusted_host: Option<&TrustedHostAdmission>,
) -> Result<Option<ResolvedSandbox>, DispatchError> {
    match trusted_host {
        Some(_) => Ok(None),
        None => host.resolve_executor_sandbox(provider, fs_profile, subprocess_cwd),
    }
}

/// Prepare the resolved sandbox for spawn, or record the trusted-host absence
/// of one.
pub(super) fn prepare_dispatch_sandbox<'a>(
    trusted_host: Option<&TrustedHostAdmission>,
    resolved_sandbox: Option<&'a ResolvedSandbox>,
) -> Result<PreparedSandbox<'a>, DispatchError> {
    Ok(match trusted_host {
        Some(_) => PreparedSandbox::none_trusted_host(),
        None => prepare_sandbox_for_dispatch(resolved_sandbox)
            .map_err(|error| DispatchError::CliInvocationPermanent(error.message))?,
    })
}
