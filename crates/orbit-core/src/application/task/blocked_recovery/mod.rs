//! The blocked-task recovery backstop.
//!
//! Some failures block a task where no pipeline hook sees them: run
//! finalization (`workflow_run_failed`, `workflow_run_interrupted`), a failed
//! claim settlement (`claim_failed`), and a gate or auto run that fails before
//! its leaf starts (a `workflow_run_failed` naming `task_gate_pipeline` or
//! `task_auto_pipeline`). On the owner, each clock tick finds those blocks and
//! dispatches `blocked_task_recovery_pipeline` for each, which runs the
//! `final_recovery` activity and applies its decision through
//! [`OrbitRuntime::apply_final_recovery`] — the same applier, crew pool and
//! requeue bound as every other final-recovery caller.
//!
//! A *block episode* is the history entry that moved the task to `blocked`.
//! Each episode gets at most one recovery, and none when:
//!
//! - a final-recovery decision was already recorded on or after the block;
//! - someone other than Orbit's own automation commented on the task,
//!   changed its fields or status, or attached an artifact after the block —
//!   a human's attributed call stands;
//! - the task was written after the block by a change no actor is recorded
//!   for — a legacy field-only write may leave no history, so the backstop
//!   cannot prove it was not a human's and leaves the task alone;
//! - a recovery run for the episode already exists (the episode key is also
//!   the run's admission key, so two ticks cannot admit two runs);
//! - the block is older than [`MAX_EPISODE_AGE_HOURS`];
//! - the blocking note carries the validation-environment marker: required
//!   validation lacked a tool, which no task decision fixes [ORB-13987].
//!
//! The backstop never resumes: `resume` is escalated, and `requeue` is how
//! work restarts. Its agent is told so: the recovery input lists only the
//! decisions this lane applies ([`BACKSTOP_DECISIONS`]) and states that its
//! checkout is discarded. The input and every escalation name the failed run's
//! retained candidate — its worktree and changed paths, read without touching
//! them — which is where any unfinished work still is. A recovery run that ends
//! without applying a decision — the agent crashed, timed out or the worker
//! died — is settled on a later tick as an escalation, so every dispatched
//! episode ends with one recorded decision.
//!
//! Enabled when `workflow.purpose_crews.final_recovery` is non-empty and this runtime
//! owns the workspace's task records; `workflow.purpose_crews.final_recovery = []` turns it off.

mod backstop;
mod episode;
mod input;
mod next_step;
mod readout;
pub(in crate::application::task) use readout::block_json;
pub(crate) mod steps;
mod triage;

pub use episode::{
    BlockEpisode, BlockSource, EpisodeDisposition, FinalRecoveryRecord, episode_disposition,
};
pub(crate) use input::BlockedRecoveryPreparation;
pub use input::{BlockedRecoveryInput, BlockedRecoveryTick, BlockedRecoveryView};
pub use next_step::BlockedNextStep;
pub use triage::{
    BLOCKED_TASK_TRIAGED_EVENT, BlockTriageDecision, BlockTriageRequest, BlockedTriageView,
    TriageBasis, TriageCandidacy, TriageEpisode, TriageOption, triage_candidacy,
};

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

/// The job the backstop dispatches, one run per block episode.
pub const BLOCKED_TASK_RECOVERY_JOB: &str = "blocked_task_recovery_pipeline";
/// Recovery runs live at once; further episodes wait for a later tick.
pub const MAX_ACTIVE_BLOCKED_RECOVERIES: usize = 2;
/// Blocks older than this are left to a human. It also bounds the first tick
/// after an upgrade, which would otherwise recover every historical block.
pub const MAX_EPISODE_AGE_HOURS: i64 = 72;
/// How far a task's `updated_at` may trail its newest attributed write and
/// still be that write: an appended comment or history entry is stamped just
/// before the record is rewritten.
pub(crate) const ATTRIBUTION_SLACK_MS: i64 = 500;
/// Run-input field holding the episode key; the keyed admission matches it.
const EPISODE_KEY_FIELD: &str = "episode_key";
/// Newest recovery runs the keyed admission and the tick look through.
const RUN_SCAN_LIMIT: usize = 500;
const TRIGGER_NAME: &str = "blocked-task-recovery";
const TRIGGER_CONSUMER: &str = "clock-sweep";
/// The `final_recovery` decisions the backstop applies. `resume` is absent: a
/// blocked task has no live run to resume.
pub(crate) const BACKSTOP_DECISIONS: [&str; 5] = [
    "complete_no_diff",
    "reject",
    "archive",
    "requeue",
    "escalate",
];
/// What the backstop's lane cannot do, stated in its agent's input.
pub(crate) const BACKSTOP_LANE_CONTRACT: &str = "Blocked-task backstop: the failed run is over and \
    nothing resumes it. Your checkout is a fresh detached checkout of base_sha that is removed \
    after your decision; nothing you change in it is committed or delivered, so do not implement \
    or validate the task there. `resume` is not available and is applied as `escalate`. When the \
    task's work should continue, escalate naming `retained_candidate`, or requeue for a fresh \
    attempt. `retained_candidate` is evidence only: never modify it.";

/// The checkout a recovery run's agent inspects: a detached linked worktree of
/// the base, private to the run and removed when its decision is applied.
pub(crate) fn recovery_checkout_path(
    state_dir: &Path,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(OrbitError::InvalidInput(format!(
            "recovery run id '{run_id}' cannot name a checkout"
        )));
    }
    Ok(state_dir.join("recovery-checkouts").join(run_id))
}

#[cfg(test)]
mod tests;
