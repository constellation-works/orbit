//! The next step for a task a failed run left `blocked`.
//!
//! `blocked` is a dead end for automation: the ship sweep and the auto drain
//! skip it until an operator acts. The history note names the failed run but
//! not what to do about it, so the task readout states the command.

use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::JobRunState;
use serde::Serialize;

use crate::OrbitRuntime;

/// The status events a run failure records when it blocks a task.
const RUN_BLOCK_EVENTS: [&str; 2] = ["workflow_run_failed", "workflow_run_interrupted"];

/// What an operator can do about a task blocked by a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlockedNextStep {
    /// The run that blocked the task.
    pub run_id: String,
    /// Set when that run can be resumed, so the failed work retries in place.
    pub resume_command: Option<String>,
    /// Returns the task to the queue for a fresh run.
    pub requeue_command: String,
}

/// The run id a blocking history note names (`run_id=<id>,`).
fn run_id_in_note(note: &str) -> Option<&str> {
    let rest = note.split_once("run_id=")?.1;
    let id = rest.split([',', ' ', ';']).next()?;
    (!id.is_empty()).then_some(id)
}

/// The run behind the task's current block: the newest history entry that
/// moved it to `blocked`, when a run failure wrote it, else the run the task
/// is still stamped with.
fn blocking_run_id(task: &Task, history: &[TaskHistoryEntry]) -> Option<String> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
    if !RUN_BLOCK_EVENTS.contains(&entry.event.as_str()) {
        return None;
    }
    entry
        .note
        .as_deref()
        .and_then(run_id_in_note)
        .map(str::to_string)
        .or_else(|| task.job_run_id.clone())
}

fn is_resumable(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
    )
}

impl OrbitRuntime {
    /// Guidance for a `blocked` task, or `None` for any other status or a
    /// block no run failure caused. Reads the run without reconciling it: a
    /// task read must not change state.
    pub fn blocked_next_step(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> Option<BlockedNextStep> {
        if task.status != TaskStatus::Blocked {
            return None;
        }
        let run_id = blocking_run_id(task, history)?;
        let resumable = self.task_run_is_local(task)
            && self
                .show_job_run_observed(&run_id)
                .is_ok_and(|run| is_resumable(run.state));
        Some(BlockedNextStep {
            resume_command: resumable.then(|| format!("orbit job resume {run_id}")),
            requeue_command: format!("orbit task update {} --status backlog", task.id),
            run_id,
        })
    }
}
