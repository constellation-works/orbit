//! The next step for a task a failed run left `blocked`.
//!
//! `blocked` is a dead end for automation: the ship sweep and the auto drain
//! skip it until an operator acts. The history note names the failed run but
//! not what to do about it, so `orbit task show` states the command.

use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

/// The status events a run failure records when it blocks a task.
const RUN_BLOCK_EVENTS: [&str; 2] = ["workflow_run_failed", "workflow_run_interrupted"];

/// What an operator can do about a task blocked by a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockedNextStep {
    /// The run that blocked the task.
    pub(crate) run_id: String,
    /// Set when that run can be resumed, so the failed work retries in place.
    pub(crate) resume_command: Option<String>,
    /// Returns the task to the queue for a fresh run.
    pub(crate) requeue_command: String,
}

impl BlockedNextStep {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "run_id": self.run_id,
            "resume_command": self.resume_command,
            "requeue_command": self.requeue_command,
        })
    }

    pub(crate) fn line(&self) -> String {
        match &self.resume_command {
            Some(resume) => format!(
                "run `{resume}` to retry the failed work, or move the task back to backlog with `{}`",
                self.requeue_command
            ),
            None => format!(
                "run {} cannot be resumed; move the task back to backlog with `{}` to run it again",
                self.run_id, self.requeue_command
            ),
        }
    }
}

/// The run id a blocking history note names (`run_id=<id>,`).
pub(crate) fn run_id_in_note(note: &str) -> Option<&str> {
    let rest = note.split_once("run_id=")?.1;
    let id = rest.split([',', ' ', ';']).next()?;
    (!id.is_empty()).then_some(id)
}

/// The run behind the task's current block: the newest history entry that
/// moved it to `blocked`, when a run failure wrote it, else the run the task
/// is still stamped with.
pub(crate) fn blocking_run_id(task: &Task, history: &[TaskHistoryEntry]) -> Option<String> {
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

/// Guidance for a `blocked` task, or `None` for any other status or a block
/// no run failure caused. Reads the run without reconciling it: `show` must
/// not change state.
pub(crate) fn blocked_next_step(
    runtime: &OrbitRuntime,
    task: &Task,
    history: &[TaskHistoryEntry],
) -> Option<BlockedNextStep> {
    if task.status != TaskStatus::Blocked {
        return None;
    }
    let run_id = blocking_run_id(task, history)?;
    let resumable = runtime
        .show_job_run_observed(&run_id)
        .is_ok_and(|run| is_resumable(run.state));
    Some(next_step(&task.id, run_id, resumable))
}

pub(crate) fn next_step(task_id: &str, run_id: String, resumable: bool) -> BlockedNextStep {
    BlockedNextStep {
        resume_command: resumable.then(|| format!("orbit job resume {run_id}")),
        requeue_command: format!("orbit task update {task_id} --status backlog"),
        run_id,
    }
}
