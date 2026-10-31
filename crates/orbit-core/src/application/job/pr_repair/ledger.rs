use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    JobRun, PrAutoFixBlocker, PrAutoFixClaim, PrAutoFixDecision, decide_pr_auto_fix,
    pr_auto_fix_attempts,
};
use serde::{Deserialize, Serialize};

use super::admission::pr_auto_fix_budget;
use super::adoption::Verdict;
use crate::OrbitRuntime;
use crate::application::task::SYSTEM_ACTOR_LABEL;

/// Task history event recording one out-of-run auto-fix a drain or the
/// dashboard started.
pub(crate) const PR_AUTO_FIX_OUT_OF_RUN_EVENT: &str = "pr_auto_fix_out_of_run";

/// One out-of-run auto-fix, as recorded in the task's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OutOfRunStart {
    pub(super) pr_number: String,
    pub(super) head_sha: String,
    pub(super) blocker: PrAutoFixBlocker,
    pub(super) source_run_id: String,
    pub(super) resumed_run_id: String,
    /// The drain that started it; absent for a dashboard start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) drain_run_id: Option<String>,
}

/// Every out-of-run auto-fix recorded for `pr_number`.
pub(super) fn out_of_run_starts(
    history: &[TaskHistoryEntry],
    pr_number: &str,
) -> Vec<OutOfRunStart> {
    history
        .iter()
        .filter(|entry| entry.event == PR_AUTO_FIX_OUT_OF_RUN_EVENT)
        .filter_map(|entry| serde_json::from_str::<OutOfRunStart>(entry.note.as_deref()?).ok())
        .filter(|start| start.pr_number == pr_number)
        .collect()
}

/// When a run last moved, as the store's recency order reads it.
pub(super) fn run_recency(run: &JobRun) -> chrono::DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// `Err` with the skip when the pull request's ledger would not admit a
/// repair of `head_sha` for `blocker`, or an out-of-run auto-fix already
/// resumed delivery for it, or for as many heads as the budget allows.
pub(super) fn ledger_admits(
    history: &[TaskHistoryEntry],
    pr_number: &str,
    head_sha: &str,
    blocker: PrAutoFixBlocker,
) -> Result<(), Verdict> {
    let budget = pr_auto_fix_budget(history, pr_number)
        .map_err(|error| Verdict::Skip("store_unreadable", error.to_string()))?;
    let attempts = pr_auto_fix_attempts(history, pr_number);
    let claim = PrAutoFixClaim {
        pr_number,
        head_sha,
        blocker,
        run_id: "",
    };
    match decide_pr_auto_fix(&attempts, &claim, budget, "", "") {
        PrAutoFixDecision::Admit(_) | PrAutoFixDecision::Continue(_) => {}
        PrAutoFixDecision::Duplicate(previous) => {
            return Err(Verdict::Skip(
                "already_repaired",
                format!(
                    "run {} already repaired {} on {head_sha} (attempt {} of {})",
                    previous.run_id,
                    blocker.as_str(),
                    previous.attempt,
                    previous.budget
                ),
            ));
        }
        PrAutoFixDecision::Exhausted(attempts) => {
            return Err(Verdict::Skip(
                "budget_exhausted",
                format!("#{pr_number} spent its {} auto-fix repairs", attempts.len()),
            ));
        }
    }
    let starts = out_of_run_starts(history, pr_number);
    if let Some(start) = starts
        .iter()
        .find(|start| start.head_sha == head_sha && start.blocker == blocker)
    {
        return Err(Verdict::Skip(
            "already_started",
            format!(
                "run {} already resumed delivery to repair {} on {head_sha}",
                start.resumed_run_id,
                blocker.as_str()
            ),
        ));
    }
    if u32::try_from(starts.len()).unwrap_or(u32::MAX) >= budget {
        return Err(Verdict::Skip(
            "budget_exhausted",
            format!(
                "#{pr_number} was handed back to delivery {} times, its auto-fix budget",
                starts.len()
            ),
        ));
    }
    Ok(())
}

impl OrbitRuntime {
    /// Record `start` in the task's history, which bounds later passes. A
    /// lost record is logged: the resumed run's own ledger admission still
    /// bounds its repair.
    pub(super) fn record_out_of_run_start(&self, task_id: &str, start: &OutOfRunStart) {
        let result = serde_json::to_string(start)
            .map_err(|error| OrbitError::Execution(error.to_string()))
            .and_then(|note| {
                self.apply_task_automation_update(
                    task_id,
                    TaskAutomationUpdate {
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: SYSTEM_ACTOR_LABEL.to_string(),
                            event: PR_AUTO_FIX_OUT_OF_RUN_EVENT.to_string(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..TaskAutomationUpdate::default()
                    },
                )
                .map(|_| ())
            });
        if let Err(error) = result {
            tracing::warn!(
                target: "orbit.core.drain",
                task_id = %task_id,
                resumed_run_id = %start.resumed_run_id,
                error = %error,
                "could not record the out-of-run auto-fix in task history",
            );
        }
    }
}
