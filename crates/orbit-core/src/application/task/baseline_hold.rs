//! Whether a backlog task is still held for a red base [ORB-14258].
//!
//! A delivery whose required command fails on its base exactly as on the
//! candidate moves the task to `backlog` under a `baseline_red_hold` history
//! entry — written by the failure handoff, by run finalization, or by the
//! owner when a follower releases its claim for that reason. Both admission
//! paths ask [`OrbitRuntime::standing_baseline_hold`] before handing the task
//! out: the local drain's backlog snapshot and the owner's pull admission.
//!
//! The hold is the task's latest status decision. Any later status change —
//! an operator moving the task, another run admitting it — supersedes it.
//! Whether it still stands is the engine's
//! [`baseline_hold_status`](orbit_engine::baseline_hold_status): it does while
//! the base ref still points at the red commit, or at one this host has seen
//! fail the same command.

use orbit_common::OrbitError;
use orbit_engine::{BaselineHoldStatus, baseline_hold_status};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, BaselineRedHold};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Why `task` is withheld from admission for a red base, or `None` when
    /// no hold stands.
    pub(crate) fn standing_baseline_hold(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        if task.status != TaskStatus::Backlog {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        let Some(hold) = latest_baseline_hold(&history) else {
            return Ok(None);
        };
        Ok(match baseline_hold_status(&self.paths().repo_root, &hold) {
            BaselineHoldStatus::Holding(why) => Some(why),
            BaselineHoldStatus::Lifted(why) => {
                tracing::debug!(task_id = %task.id, why, "baseline red hold lifted");
                None
            }
        })
    }
}

/// The hold the task's latest status decision put it under, if that decision
/// was a baseline-red hold.
fn latest_baseline_hold(history: &[TaskHistoryEntry]) -> Option<BaselineRedHold> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some())?;
    if entry.event != BASELINE_RED_HOLD_EVENT || entry.to_status != Some(TaskStatus::Backlog) {
        return None;
    }
    BaselineRedHold::from_text(entry.note.as_deref()?)
}
