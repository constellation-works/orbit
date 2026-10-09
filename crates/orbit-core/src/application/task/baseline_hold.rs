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
//! or admission path [ORB-14739], nor inside the clock tick, which holds the
//! host's sweep lock [ORB-14823]. The tick
//! ([`OrbitRuntime::run_baseline_hold_tick`]) judges each standing hold only
//! from recorded base results. When a hold's base moved to a tip nobody has
//! checked, it dispatches one detached [`BASELINE_HOLD_REFRESH_JOB`] run per
//! workspace at a time, whose step ([`OrbitRuntime::refresh_baseline_holds`])
//! runs the command. An inconclusive attempt suppresses further refreshes
//! for that tip and command for 15 minutes, or until the base moves.
//! Either records the verdict as a
//! `baseline_red_hold_verdict` history entry, which changes no status.
//! Admission reads only the latest verdict after the hold. A hold with no
//! verdict yet stays held until one is recorded.

use std::time::Instant;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{
    BaselineHoldStatus, DispatchError, baseline_hold_status, recorded_baseline_hold_status,
};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{BASELINE_RED_HOLD_EVENT, BaselineRedHold, JobRunState, JobRunTrigger};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::OrbitRuntime;
use crate::application::job::pipeline::{PipelineSubmission, ROUTINE_DISPATCH_ORBIT_DIR_FIELD};

/// The job a clock tick dispatches to check holds whose base moved to a tip
/// with no recorded result; at most one run per workspace is live.
pub const BASELINE_HOLD_REFRESH_JOB: &str = "baseline_hold_refresh_pipeline";
/// Trigger recorded on each refresh run the tick submits.
const TRIGGER_NAME: &str = "baseline-hold-refresh";
const TRIGGER_CONSUMER: &str = "clock-sweep";
/// Recent refresh runs scanned for a live one.
const RUN_SCAN_LIMIT: usize = 20;

/// History event recording the latest check of a standing hold.
const BASELINE_RED_HOLD_VERDICT_EVENT: &str = "baseline_red_hold_verdict";

/// A check of a standing [`BaselineRedHold`], persisted as the
/// note of a [`BASELINE_RED_HOLD_VERDICT_EVENT`] entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BaselineHoldVerdict {
    /// The command passed on a new base tip, so admission may hand the task out.
    lifted: bool,
    /// Why, naming the base tip checked and the command.
    reason: String,
}

/// What one check of the standing baseline-red holds did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BaselineHoldRefresh {
    /// Tasks whose hold the check lifted.
    pub lifted: Vec<String>,
    /// Tasks still held, with a newly recorded verdict.
    pub held: Vec<String>,
    /// Tasks whose base moved to a tip with no recorded result, left to a
    /// refresh run. Only the clock tick leaves any.
    pub unchecked: Vec<String>,
    /// The refresh run the clock tick started for `unchecked`, if any.
    pub dispatched: Option<String>,
    /// Why the check did nothing, when it stood down.
    pub skipped: Option<String>,
}

impl OrbitRuntime {
    /// Why `task` is withheld from admission for a red base, or `None` when
    /// no hold stands. Reads only the task's history: the verdict is the
    /// clock tick's or its refresh run's, never computed here.
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

    /// One clock tick's check of the standing baseline-red holds
    /// [ORB-14823]. It judges each hold only from recorded base results and
    /// never runs a required command, so it stays short while the tick holds
    /// the host's sweep lock. When some hold's base moved to a tip with no
    /// recorded result, it dispatches one [`BASELINE_HOLD_REFRESH_JOB`] run to
    /// check it, unless one is already live. No new hold is read after
    /// `deadline`; the next tick resumes.
    pub fn run_baseline_hold_tick(
        &self,
        deadline: Instant,
    ) -> Result<BaselineHoldRefresh, OrbitError> {
        let repo = &self.paths().repo_root;
        let mut refresh = self.check_baseline_holds(Some(deadline), &|hold| {
            recorded_baseline_hold_status(repo, hold)
        })?;
        if refresh.unchecked.is_empty() || self.baseline_hold_refresh_live()? {
            return Ok(refresh);
        }
        let input = json!({
            ROUTINE_DISPATCH_ORBIT_DIR_FIELD: self.shared_root().to_string_lossy(),
        });
        let submission = PipelineSubmission {
            trigger: JobRunTrigger::state_routine(TRIGGER_NAME, TRIGGER_CONSUMER),
            ..PipelineSubmission::catalog(
                BASELINE_HOLD_REFRESH_JOB,
                input,
                Some(SYSTEM_ACTOR_LABEL),
            )
        };
        let (result, _) = self.submit_keyed_pipeline_run(submission)?;
        refresh.dispatched = Some(result.run_id);
        Ok(refresh)
    }

    /// Re-evaluate every backlog task's standing baseline-red hold and record
    /// each changed verdict: the step of a [`BASELINE_HOLD_REFRESH_JOB`] run.
    ///
    /// The check may run the hold's required command on a new base tip (once
    /// per tip and command for a conclusive result; inconclusive attempts
    /// retry after a 15-minute back-off), so it runs only in
    /// that detached run, never on a read or admission path or inside the
    /// clock tick. Holds sharing a base and command share that run. No new
    /// hold is started after `deadline`.
    pub fn refresh_baseline_holds(
        &self,
        deadline: Option<Instant>,
    ) -> Result<BaselineHoldRefresh, OrbitError> {
        let repo = &self.paths().repo_root;
        self.check_baseline_holds(deadline, &|hold| {
            Some(baseline_hold_status(self, repo, hold))
        })
    }

    /// Judge every standing, unlifted hold with `status` and record each
    /// changed verdict. A hold `status` cannot judge is listed `unchecked`.
    fn check_baseline_holds(
        &self,
        deadline: Option<Instant>,
        status: &dyn Fn(&BaselineRedHold) -> Option<BaselineHoldStatus>,
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
            let verdict = match status(&standing.hold) {
                None => {
                    refresh.unchecked.push(task.id.clone());
                    continue;
                }
                Some(BaselineHoldStatus::Holding(reason)) => BaselineHoldVerdict {
                    lifted: false,
                    reason,
                },
                Some(BaselineHoldStatus::Lifted(reason)) => BaselineHoldVerdict {
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

    /// Whether a [`BASELINE_HOLD_REFRESH_JOB`] run is still live here.
    fn baseline_hold_refresh_live(&self) -> Result<bool, OrbitError> {
        Ok(self
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(BASELINE_HOLD_REFRESH_JOB.to_string()),
                limit: Some(RUN_SCAN_LIMIT),
                include_steps: false,
                ..JobRunQuery::default()
            })?
            .iter()
            .any(|run| !run.state.is_terminal() && run.state != JobRunState::Skipped))
    }

    /// Append `verdict` to `task_id`'s history if `hold` is still its standing
    /// hold. `false` when a later status decision superseded it, or when the
    /// latest verdict already says the same: the tick and a refresh run may
    /// judge one hold at once.
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
                if current.is_none_or(|current| {
                    current.hold != *hold || current.verdict.as_ref() == Some(verdict)
                }) {
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

/// `refresh_baseline_holds`: the only step of [`BASELINE_HOLD_REFRESH_JOB`].
/// Re-checks every standing hold, running a required command where a base
/// tip has no recorded result.
pub(crate) fn refresh_baseline_holds_step(
    runtime: &OrbitRuntime,
    action: &str,
) -> Result<Value, DispatchError> {
    let refresh = runtime.refresh_baseline_holds(None).map_err(|error| {
        DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: error.to_string(),
        }
    })?;
    Ok(json!({
        "lifted": refresh.lifted,
        "held": refresh.held,
        "skipped": refresh.skipped,
    }))
}
