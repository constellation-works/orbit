//! Which activities may declare `response_correction` [ORB-22426].
//!
//! A correction is one more invocation of the whole activity. Only the prompt
//! asks it to change nothing but its reply. Admission therefore requires the
//! read-only filesystem profile and denies the built-in Orbit mutation tools
//! below, including coordination writes outside the worktree. That denial
//! also covers a task's `required_tools` at run time: the runtime grant path
//! drops a mutating requirement with an omitted-requirement note instead of
//! adding it to the effective grant of a correction activity.

use super::activity_v2::{ActivityV2, ActivityV2Spec};
use super::tool_allowlist::tool_allowed;

/// The filesystem profile that denies writes to the agent's worktree.
const READ_ONLY_FS_PROFILE: &str = "reviewer";

/// Registered built-in Orbit tools classified as mutating, including writes
/// to coordination state outside the worktree, as policy names: a family's
/// mutating actions appear as `family:action`. Keep this table and the
/// shipped correction activities' disallow lists in sync with the tool
/// registry.
/// File and process access is separately confined by the filesystem profile.
pub const MUTATING_TOOLS: &[&str] = &[
    "orbit.auto_task:add",
    "orbit.auto_task:mint",
    "orbit.auto_task:update",
    "orbit.command.exec",
    "orbit.decision.digest",
    "orbit.decision.submit",
    "orbit.decision:raise",
    "orbit.drain.claim.bind",
    "orbit.drain.claim.effort",
    "orbit.drain.claim.settle",
    "orbit.evolution.add",
    "orbit.experiment.abandon",
    "orbit.experiment.assign",
    "orbit.experiment.clone",
    "orbit.experiment.conclude",
    "orbit.experiment.create",
    "orbit.experiment.pause",
    "orbit.experiment.resume",
    "orbit.experiment.reweight",
    "orbit.experiment.start",
    "orbit.experiment.update",
    "orbit.friction:add",
    "orbit.friction:update",
    "orbit.memory.propose",
    "orbit.memory.write",
    "orbit.notice.close",
    "orbit.notice.post",
    "orbit.pipeline.invoke",
    "orbit.pipeline.wait",
    "orbit.resident.checkpoint",
    "orbit.resident.control",
    "orbit.resident.grant",
    "orbit.resident.relay",
    "orbit.routine.control",
    "orbit.run:resume",
    "orbit.task.add",
    "orbit.task.artifact:put",
    "orbit.task.block_triage",
    "orbit.task.pull",
    "orbit.task.recover:reconcile_review",
    "orbit.task.recover:recover_claim",
    "orbit.task.recover:repilot",
    "orbit.task.recover:requeue",
    "orbit.task.recover:retry_handoff",
    "orbit.task.recover:review_reset",
    "orbit.task.recover:revoke_handoff",
    "orbit.task.update",
    "orbit.workflow:auto",
    "orbit.workflow:land",
    "orbit.workflow:ship",
];

/// Whether `tool` covers a built-in Orbit mutation a `response_correction`
/// activity must never be granted, however the grant was composed. A family
/// covers each of its mutating actions.
pub fn is_response_correction_mutating_tool(tool: &str) -> bool {
    let tool = [tool.to_string()];
    MUTATING_TOOLS
        .iter()
        .any(|mutating| tool_allowed(mutating, &tool))
}

/// Why an activity may not declare `response_correction`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResponseCorrectionActivityError {
    /// Nothing reads the reply, so there is nothing to correct.
    #[error("it needs `require_response_envelope: true`")]
    ResponseEnvelopeNotRequired,
    /// A write could be repeated.
    #[error("it needs `fsProfile: {READ_ONLY_FS_PROFILE}`, so the activity cannot write files")]
    NotReadOnlyFilesystem,
    /// Unfinished work and a rejected reply would compete for one budget.
    #[error("it cannot be combined with `incomplete_continuation`")]
    DeclaresContinuation,
    /// The unsandboxed mode has no read-only guarantee.
    #[error("it cannot be combined with `trustedHostExecution`")]
    TrustedHost,
    /// An Orbit state change or dispatched operation could be repeated.
    #[error("the activity must not be granted the mutating tool `{tool}`")]
    MutatingTool {
        /// The granted tool.
        tool: &'static str,
    },
}

/// Refuse `response_correction` unless workspace writes and the built-in
/// Orbit mutations above are denied. Called from asset load in either grant mode.
pub fn validate_response_correction_activity(
    activity: &ActivityV2,
) -> Result<(), ResponseCorrectionActivityError> {
    let ActivityV2Spec::AgentLoop(spec) = &activity.spec else {
        return Ok(());
    };
    if spec.response_correction.is_none() {
        return Ok(());
    }
    if !spec.require_response_envelope {
        return Err(ResponseCorrectionActivityError::ResponseEnvelopeNotRequired);
    }
    if activity.fs_profile.as_deref() != Some(READ_ONLY_FS_PROFILE) {
        return Err(ResponseCorrectionActivityError::NotReadOnlyFilesystem);
    }
    if spec.incomplete_continuation.is_some() {
        return Err(ResponseCorrectionActivityError::DeclaresContinuation);
    }
    if spec.trusted_host_execution {
        return Err(ResponseCorrectionActivityError::TrustedHost);
    }
    let granted = |tool: &str| match &spec.tool_disallow_list {
        Some(disallowed) => !tool_allowed(tool, disallowed),
        None => tool_allowed(tool, &spec.tools),
    };
    if let Some(tool) = MUTATING_TOOLS.iter().copied().find(|tool| granted(tool)) {
        return Err(ResponseCorrectionActivityError::MutatingTool { tool });
    }
    Ok(())
}
