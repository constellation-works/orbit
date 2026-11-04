//! Who owns the assignment's status across a mandate's life: core, never the
//! cycle's agent (its own run holds the task, and the lifecycle refuses a
//! run-owned status change).
//!
//! - **Active.** A valid mandate puts its assignment in `in-progress`: when the
//!   dashboard creates it, when `orbit task artifact put` attaches it, and at
//!   the latest when a cycle starts. `backlog` would offer it to drains, and
//!   `proposed` to task-pilot; neither says what the mandate is doing.
//! - **Handed off.** A `handoff` checkpoint is the agent's durable request.
//!   `resident_finalize` settles it by moving the assignment to `review`, which
//!   ends the cycles; `resident_observe` settles one whose finalize never ran.
//! - **Stopped.** `orbit.resident.control stop` moves it to `review` once no
//!   cycle is live ([`OrbitRuntime::hand_off_stopped_resident`]).
//!
//! `review` is reachable only from `in-progress`, so settling walks an
//! assignment that is still elsewhere through it.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComment, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{ResidentCheckpoint, ResidentComment};

use super::{ResidentMandateState, resident_mandate_ended};
use crate::OrbitRuntime;
use crate::application::task::{TaskUpdateParams, task_status_transition_required_field};

/// The outcome a checkpoint names to hand the mandate to its human.
const RESIDENT_HANDOFF_OUTCOME: &str = "handoff";

/// The plan a system-activated assignment gets when it has none: `in-progress`
/// requires one.
const RESIDENT_ASSIGNMENT_PLAN: &str = "Run bounded resident cycles against this mandate until its exit criteria hold, its deadline passes or an operator stops it.";

/// The latest checkpoint if it asked for a handoff that has not reached
/// `review`. A move to `review` after it settled it, so an operator who reopens
/// a handed-off assignment resumes its cycles instead of being handed off again.
fn unsettled_resident_handoff(
    task_id: &str,
    comments: &[TaskComment],
    history: &[TaskHistoryEntry],
) -> Option<ResidentCheckpoint> {
    let (at, checkpoint) =
        comments.iter().rev().find_map(|comment| {
            match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(checkpoint) if checkpoint.mandate == task_id => {
                    Some((comment.at, *checkpoint))
                }
                _ => None,
            }
        })?;
    let settled = history
        .iter()
        .any(|entry| entry.to_status == Some(TaskStatus::Review) && entry.at >= at);
    (checkpoint.outcome == RESIDENT_HANDOFF_OUTCOME && !settled).then_some(checkpoint)
}

impl OrbitRuntime {
    /// Move a `proposed` or `backlog` assignment of an active mandate to
    /// `in-progress`, returning the moved task. Anything else changes nothing.
    /// Never fails its caller: the mandate still works from its old status and
    /// the next cycle retries, so a refusal is only logged.
    pub(crate) fn activate_resident_assignment(&self, task_id: &str) -> Option<Task> {
        let activate = || -> Result<Option<Task>, OrbitError> {
            let task = self.get_task(task_id)?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Ok(None);
            }
            let history = self.get_task_history(task_id)?;
            let view = self.resident_view(task.clone(), &history)?;
            if view.state().0 != ResidentMandateState::Active {
                return Ok(None);
            }
            self.resident_assignment_to_in_progress(&task).map(Some)
        };
        activate().unwrap_or_else(|error| {
            tracing::warn!(task_id, "resident assignment not activated: {error}");
            None
        })
    }

    fn resident_assignment_to_in_progress(&self, task: &Task) -> Result<Task, OrbitError> {
        let needs_plan = task_status_transition_required_field(self, task, TaskStatus::InProgress)?
            == Some("plan");
        self.update_task_as_system(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                plan: needs_plan.then(|| RESIDENT_ASSIGNMENT_PLAN.to_string()),
                ..Default::default()
            },
            None,
        )
    }

    /// Move the assignment to `review` from any open status. `summary` is
    /// written with the move. Returns `review`, `already`, or `refused: <why>`
    /// when the lifecycle does not allow it.
    pub(super) fn move_resident_assignment_to_review(
        &self,
        task_id: &str,
        summary: Option<String>,
    ) -> Result<String, OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status == TaskStatus::Review || resident_mandate_ended(task.status) {
            return Ok("already".into());
        }
        let walked = if task.status == TaskStatus::InProgress {
            Ok(task)
        } else {
            self.resident_assignment_to_in_progress(&task)
        };
        // Deterministic machinery: the lifecycle table still applies, but the
        // cycle's own run (finalize runs inside it) must not block the move.
        let moved = walked.and_then(|_| {
            self.update_task_as_system(
                task_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Review),
                    execution_summary: summary,
                    ..Default::default()
                },
                None,
            )
        });
        Ok(match moved {
            Ok(_) => "review".into(),
            Err(error) => format!("refused: {error}"),
        })
    }

    /// Settle a `handoff` checkpoint that has not reached `review`, keeping an
    /// execution summary the agent wrote and otherwise summarizing from the
    /// checkpoint. Returns what happened, or `None` when none is unsettled.
    pub(super) fn settle_resident_handoff(
        &self,
        task_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let comments = self.get_task_comments(task_id)?;
        let history = self.get_task_history(task_id)?;
        let Some(checkpoint) = unsettled_resident_handoff(task_id, &comments, &history) else {
            return Ok(None);
        };
        let task = self.get_task(task_id)?;
        let summary = task.execution_summary.trim().is_empty().then(|| {
            format!(
                "Resident handed off in cycle {}.\n\n{}",
                checkpoint.cycle, checkpoint.notes
            )
        });
        self.move_resident_assignment_to_review(task_id, summary)
            .map(Some)
    }
}
