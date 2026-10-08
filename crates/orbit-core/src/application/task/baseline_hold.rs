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
//!
//! Whether it still stands is the engine's
//! [`baseline_hold_status`](orbit_engine::baseline_hold_status), which may run
//! the whole required command on a new base tip. That never happens on a read
//! or admission path [ORB-14739]: the owner's clock tick
//! ([`OrbitRuntime::refresh_baseline_holds`]) re-evaluates each standing hold
//! and records the verdict as a `baseline_red_hold_verdict` history entry,
//! which changes no status. Admission reads only the latest verdict after the
//! hold. A hold with no verdict yet stays held until the tick records one.

use std::time::Instant;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{BaselineHoldStatus, baseline_hold_status};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, BaselineRedHold};
use serde::{Deserialize, Serialize};

use super::TaskRecordUpdateParams;
use crate::OrbitRuntime;

/// History event recording the clock tick's latest check of a standing hold.
const BASELINE_RED_HOLD_VERDICT_EVENT: &str = "baseline_red_hold_verdict";

/// The clock tick's check of a standing [`BaselineRedHold`], persisted as the
/// note of a [`BASELINE_RED_HOLD_VERDICT_EVENT`] entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineHoldVerdict {
    /// The command passed on a new base tip, so admission may hand the task out.
    lifted: bool,
    /// Why, naming the base tip checked and the command.
    reason: String,
}

/// What one clock tick did about standing baseline-red holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BaselineHoldRefresh {
    /// Tasks whose hold the tick lifted.
    pub lifted: Vec<String>,
    /// Tasks still held, with a newly recorded verdict.
    pub held: Vec<String>,
    /// Why the tick did nothing, when it stood down.
    pub skipped: Option<String>,
}

impl OrbitRuntime {
    /// Why `task` is withheld from admission for a red base, or `None` when
    /// no hold stands. Reads only the task's history: the verdict is the
    /// clock tick's, never computed here.
    pub(crate) fn standing_baseline_hold(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        if task.status != TaskStatus::Backlog {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        let Some(standing) = StandingHold::latest(&history) else {
            return Ok(None);
        };
        Ok(match standing.verdict {
            Some(verdict) if verdict.lifted => None,
            Some(verdict) => Some(verdict.reason),
            None => Some(format!(
                "required validation `{}` fails on `{}` at {}; the hold stands until the owner's \
                 clock checks a new base tip and the command passes there",
                standing.hold.command,
                if standing.hold.base_ref.is_empty() {
                    "the base"
                } else {
                    standing.hold.base_ref.as_str()
                },
                standing.hold.base_sha
            )),
        })
    }

    /// One clock tick: re-evaluate every backlog task's standing baseline-red
    /// hold and record each changed verdict [ORB-14739].
    ///
    /// The check may run the hold's required command on a new base tip (once
    /// per tip and command: the engine caches the result), so it stays off
    /// read and admission paths. Holds sharing a base and command share that
    /// run. No new hold is started after `deadline`; the next tick resumes.
    pub fn refresh_baseline_holds(
        &self,
        deadline: Option<Instant>,
    ) -> Result<BaselineHoldRefresh, OrbitError> {
        let mut refresh = BaselineHoldRefresh::default();
        if self.worker_invocation().is_some() {
            refresh.skipped =
                Some("a claimed worker never checks baseline holds; its owner does".to_string());
            return Ok(refresh);
        }
        if let Some(owner) = self.coordination_write_owner() {
            refresh.skipped = Some(format!(
                "this replica checkout does not own its task records; machine '{owner}' checks \
                 their baseline holds"
            ));
            return Ok(refresh);
        }
        for task in
            self.list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)?
        {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            let Some(standing) = StandingHold::latest(&self.get_task_history(&task.id)?) else {
                continue;
            };
            if standing
                .verdict
                .as_ref()
                .is_some_and(|verdict| verdict.lifted)
            {
                continue;
            }
            let verdict = match baseline_hold_status(self, &self.paths().repo_root, &standing.hold)
            {
                BaselineHoldStatus::Holding(reason) => BaselineHoldVerdict {
                    lifted: false,
                    reason,
                },
                BaselineHoldStatus::Lifted(reason) => BaselineHoldVerdict {
                    lifted: true,
                    reason,
                },
            };
            if standing.verdict.as_ref() == Some(&verdict) {
                continue;
            }
            match self.record_baseline_hold_verdict(&task.id, &standing.hold, &verdict) {
                Ok(false) => {}
                Ok(true) if verdict.lifted => {
                    tracing::info!(task_id = %task.id, why = verdict.reason, "baseline red hold lifted");
                    refresh.lifted.push(task.id.clone());
                }
                Ok(true) => refresh.held.push(task.id.clone()),
                Err(error) => tracing::warn!(
                    task_id = %task.id,
                    "could not record a baseline red hold verdict: {error}"
                ),
            }
        }
        Ok(refresh)
    }

    /// Append `verdict` to `task_id`'s history if `hold` is still its standing
    /// hold. `false` when a later status decision superseded it.
    fn record_baseline_hold_verdict(
        &self,
        task_id: &str,
        hold: &BaselineRedHold,
        verdict: &BaselineHoldVerdict,
    ) -> Result<bool, OrbitError> {
        let note = serde_json::to_string(verdict)
            .map_err(|error| OrbitError::Execution(format!("encode hold verdict: {error}")))?;
        let mut recorded = false;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let current = StandingHold::latest(&self.get_task_history(task_id)?);
                if current.is_none_or(|current| current.hold != *hold) {
                    return Ok(());
                }
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: "system".into(),
                        expected_status: Some(vec![TaskStatus::Backlog]),
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: "system".into(),
                            event: BASELINE_RED_HOLD_VERDICT_EVENT.into(),
                            note: Some(note.clone()),
                            from_status: None,
                            to_status: None,
                        }],
                        ..Default::default()
                    },
                )?;
                recorded = true;
                Ok(())
            })?;
        Ok(recorded)
    }
}

/// The hold the task's latest status decision put it under, with the latest
/// verdict recorded since.
struct StandingHold {
    hold: BaselineRedHold,
    verdict: Option<BaselineHoldVerdict>,
}

impl StandingHold {
    fn latest(history: &[TaskHistoryEntry]) -> Option<Self> {
        let at = history
            .iter()
            .rposition(|entry| entry.to_status.is_some())?;
        let entry = &history[at];
        if entry.event != BASELINE_RED_HOLD_EVENT || entry.to_status != Some(TaskStatus::Backlog) {
            return None;
        }
        let hold = BaselineRedHold::from_text(entry.note.as_deref()?)?;
        let verdict = history[at + 1..]
            .iter()
            .rev()
            .filter(|entry| entry.event == BASELINE_RED_HOLD_VERDICT_EVENT)
            .find_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok());
        Some(Self { hold, verdict })
    }
}
