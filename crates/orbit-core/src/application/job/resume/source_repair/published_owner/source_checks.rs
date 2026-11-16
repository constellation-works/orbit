//! Free checks on a stopped source run: whether its publication can be
//! preserved, adopted or repaired, and the refusals when it cannot.

use super::super::*;
use super::{PRESERVED, PreservedPublication};
use orbit_common::SourceRepairRunStateRefusal;
use orbit_types::workflow::{self, HeldFailure, JobRunStep};

pub(super) fn preserved_entry(
    record: &PreservedPublication,
) -> Result<TaskHistoryEntry, OrbitError> {
    Ok(TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: PRESERVED.into(),
        note: Some(serde_json::to_string(record).map_err(|e| refuse(e.to_string()))?),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    })
}

pub(super) fn parse_preserved(
    entry: &TaskHistoryEntry,
) -> Result<PreservedPublication, OrbitError> {
    serde_json::from_str(entry.note.as_deref().unwrap_or_default())
        .map_err(|e| refuse(e.to_string()))
}

/// The worker that owned `source` is gone and the run has a finish time.
fn source_owner_is_gone(source: &JobRun) -> bool {
    source.finished_at.is_some() && run_owner_liveness(source) == RunOwnerLiveness::Stopped
}

/// A step that records the before-landing review hold: the run ended `held`
/// so a recorded decision can land the open pull request.
fn step_holds_review_decision(step: &JobRunStep) -> bool {
    HeldFailure::of(step.error_code.as_deref(), step.error_message.as_deref())
        == Some(HeldFailure::DecisionPending)
}

/// `source` is `held` because a before-landing review left its pull request
/// open for a recorded decision.
fn held_for_review_decision(source: &JobRun) -> bool {
    source.state == JobRunState::Held && source.steps.iter().any(step_holds_review_decision)
}

/// A failed delivery, or a review-decision hold, whose worker is gone.
///
/// A before-landing review refusal ends the delivery `held`, not `failed`,
/// and final recovery asks the operator about that same run. The run will
/// never become `failed`, so that conclusively stopped hold is a requeue
/// source. A delivery that is still running is not.
fn repairable_stopped_source(source: &JobRun) -> bool {
    source_owner_is_gone(source)
        && (source.state == JobRunState::Failed || held_for_review_decision(source))
}

/// Checkpoints by which a run published to the remote or the forge itself.
/// A local `commit` is not one: the next adoption re-applies the publication.
pub(super) const OWN_PUBLICATION_KEYS: [&str; 5] =
    ["push", "pr_open", "landing_push", "re_push", "re_push_2"];

/// `run` stopped `held` because its deferred footprint stayed busy
/// ([`HeldFailure::FootprintUnavailable`]): no editor started.
pub(in crate::application::job::resume::source_repair) fn footprint_held(run: &JobRun) -> bool {
    run.job_id == workflow::TASK_PR_PIPELINE_JOB
        && run.state == JobRunState::Held
        && source_owner_is_gone(run)
        && run.steps.iter().any(|step| {
            HeldFailure::of(step.error_code.as_deref(), step.error_message.as_deref())
                == Some(HeldFailure::FootprintUnavailable)
        })
}

/// A publication the next run may adopt: a stopped failed or cancelled
/// delivery, or a stopped review-decision hold.
pub(super) fn adoptable_published_source(source: &JobRun) -> bool {
    source_owner_is_gone(source)
        && (matches!(source.state, JobRunState::Failed | JobRunState::Cancelled)
            || held_for_review_decision(source))
}

/// What to tell the operator when `source` cannot be repaired, naming an
/// action that can succeed in its current state.
fn stopped_source_refusal(source: &JobRun) -> OrbitError {
    let run_id = &source.run_id;
    if !source.state.is_terminal() || source.finished_at.is_none() {
        // Cancel succeeds only while the run is not terminal. Discard of a
        // blocked task also succeeds; the requeue wrapper names that route.
        refuse_run_state(
            SourceRepairRunStateRefusal::StillRunning,
            format!(
                "delivery `{run_id}` is still running, so it cannot be repaired. \
                 Cancel it with `orbit run cancel {run_id} --confirm`"
            ),
        )
    } else if run_owner_liveness(source) != RunOwnerLiveness::Stopped {
        // The record is terminal, so cancel reports already terminal and
        // does not stop the worker. Discard is the route that can succeed.
        refuse_run_state(
            SourceRepairRunStateRefusal::StillRunning,
            format!("delivery `{run_id}` is still running, so it cannot be repaired"),
        )
    } else {
        refuse_run_state(
            SourceRepairRunStateRefusal::NotStoppedFailed,
            format!(
                "run `{run_id}` is {}, not a conclusively stopped failed delivery",
                source.state
            ),
        )
    }
}

/// Refuse unless `source` is a conclusively stopped failed delivery, or a
/// conclusively stopped run held for a before-landing review decision.
/// A delivery that is still running stays refused.
pub(in crate::application::job::resume::source_repair) fn require_stopped_failed_requeue_source(
    source: &JobRun,
) -> Result<(), OrbitError> {
    if repairable_stopped_source(source) {
        return Ok(());
    }
    Err(stopped_source_refusal(source))
}
