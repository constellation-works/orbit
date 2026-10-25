//! Delivery attention for `review` (and blocked-with-PR) tasks, derived
//! only from durable run, history and ledger state. No forge reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    JobRun, JobRunState, PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixAttempt,
    PrAutoFixPush,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::{run_display_steps, run_error_step};
use crate::application::task::TaskRow;
use crate::runtime::run_input::carried_task_ids;

#[cfg(test)]
mod tests;

const DELIVERY_JOB: &str = "task_pr_pipeline";

fn eligible(task: &Task) -> bool {
    task.status == TaskStatus::Review
        || (task.status == TaskStatus::Blocked && task.github_pr_number().is_some())
}

fn latest_attempt(
    history: &[TaskHistoryEntry],
    pr_number: Option<&str>,
) -> Option<(PrAutoFixAttempt, DateTime<Utc>)> {
    let pr_number = pr_number?;
    history
        .iter()
        .filter(|entry| entry.event == PR_AUTO_FIX_ADMITTED_EVENT)
        .filter_map(|entry| {
            let attempt: PrAutoFixAttempt = serde_json::from_str(entry.note.as_deref()?).ok()?;
            (attempt.pr_number == pr_number).then_some((attempt, entry.at))
        })
        .next_back()
}

fn latest_push_at(history: &[TaskHistoryEntry], pr_number: Option<&str>) -> Option<DateTime<Utc>> {
    let pr_number = pr_number?;
    history
        .iter()
        .filter(|entry| entry.event == PR_AUTO_FIX_PUSHED_EVENT)
        .filter_map(|entry| {
            let push: PrAutoFixPush = serde_json::from_str(entry.note.as_deref()?).ok()?;
            (push.pr_number == pr_number).then_some(entry.at)
        })
        .max()
}

fn run_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// Pure projection from already-read durable state.
pub fn delivery_attention_value(
    task: &Task,
    history: &[TaskHistoryEntry],
    newest_run: Option<&JobRun>,
    failed_step_id: Option<&str>,
    repairing: bool,
) -> Option<Value> {
    if !eligible(task) {
        return None;
    }
    let pr_number = task.github_pr_number().map(str::to_string);
    let attempt = latest_attempt(history, pr_number.as_deref());
    let run_failed = newest_run.is_some_and(|run| {
        matches!(
            run.state,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
        )
    });
    if !run_failed && attempt.is_none() {
        return None;
    }
    // A conclusive successful delivery newer than every admission and
    // published repair for this PR supersedes that ledger history. The
    // history itself is retained for budgeting; only the warning is retired.
    // A success that predates the latest ledger entry is not recovery from
    // it, and a live repair keeps the current blocker visible.
    if !repairing
        && let Some(run) = newest_run
        && run.state == JobRunState::Success
        && let Some((_, admitted_at)) = &attempt
    {
        let ledger_at = latest_push_at(history, pr_number.as_deref())
            .map_or(*admitted_at, |pushed_at| pushed_at.max(*admitted_at));
        if run_at(run) > ledger_at {
            return None;
        }
    }
    let (blocker, head_sha, attempt_n, budget, observed_at) = match &attempt {
        Some((attempt, at)) => (
            Some(attempt.blocker.as_str().to_string()),
            Some(attempt.head_sha.clone()),
            Some(attempt.attempt),
            Some(attempt.budget),
            Some(*at),
        ),
        None => (None, None, None, None, None),
    };
    let observed_at = observed_at.or_else(|| {
        newest_run.and_then(|run| run.finished_at.or(run.started_at).or(Some(run.created_at)))
    });
    Some(json!({
        "run_id": newest_run.map(|run| run.run_id.clone()),
        "run_state": run_failed.then(|| newest_run.map(|run| run.state.to_string())).flatten(),
        "failed_step_id": run_failed.then_some(failed_step_id).flatten(),
        "blocker": blocker,
        "head_sha": head_sha,
        "attempt": attempt_n,
        "budget": budget,
        "repairing": repairing,
        "pr_number": pr_number,
        "observed_at": observed_at.map(|at| at.to_rfc3339()),
        "as_of": observed_at.map(|at| at.to_rfc3339()),
    }))
}

fn failed_step_id(runtime: &OrbitRuntime, run: &JobRun) -> Option<String> {
    if let Ok(Some(state)) = runtime.read_run_state(&run.run_id)
        && let Some(checkpoint) = &state.failure_activity_checkpoint
    {
        return Some(checkpoint.failed_step_id.clone());
    }
    // Audit-derived steps carry the YAML step id (e.g. `complete_pr`).
    let display = run_display_steps(run, || {
        runtime
            .collect_run_audit_steps(&run.run_id)
            .unwrap_or_default()
    });
    let from_display = display
        .records
        .iter()
        .rev()
        .find(|step| {
            step.state != "skipped"
                && (step.error_message.is_some()
                    || matches!(step.state.as_str(), "failed" | "timeout" | "interrupted"))
        })
        .map(|step| step.target_id.clone());
    if from_display.is_some() {
        return from_display;
    }
    run_error_step(run).map(|step| step.target_id.clone())
}

fn is_live(run: &JobRun) -> bool {
    matches!(
        run.state,
        JobRunState::Pending | JobRunState::Running | JobRunState::Retrying
    )
}

impl OrbitRuntime {
    /// Attention for one task row, using the same lookup and projection as lists.
    pub fn delivery_attention(&self, row: &TaskRow) -> Result<Option<Value>, OrbitError> {
        Ok(self.delivery_attention_map([row])?.remove(&row.task.id))
    }

    /// Attention for a list page: chunked indexed ownership seeks, with an
    /// independent eight-run recency window per eligible task and batched step
    /// reads. Audit reads are only for flagged rows. No forge calls.
    pub fn delivery_attention_map<'a>(
        &self,
        rows: impl IntoIterator<Item = &'a TaskRow>,
    ) -> Result<BTreeMap<String, Value>, OrbitError> {
        let rows: Vec<&TaskRow> = rows.into_iter().filter(|row| eligible(&row.task)).collect();
        let ids = rows
            .iter()
            .map(|row| row.task.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let runs = self
            .stores()
            .jobs()
            .recent_job_runs_for_tasks(DELIVERY_JOB, &ids, 8)?;
        let mut map = BTreeMap::new();
        for row in rows {
            let runs = runs
                .get(&row.task.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let newest = runs.iter().find(|run| {
                let carried = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
                carried.len() == 1 && carried.contains(&row.task.id)
            });
            // Detail has always treated any carried live run, including a
            // multi-task repair, as repairing within this task's window.
            let repairing = runs.iter().any(is_live);
            let failed = newest.filter(|run| {
                matches!(
                    run.state,
                    JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted
                )
            });
            let step = failed.and_then(|run| failed_step_id(self, run));
            if let Some(value) = delivery_attention_value(
                &row.task,
                &row.history,
                newest,
                step.as_deref(),
                repairing,
            ) {
                map.insert(row.task.id.clone(), value);
            }
        }
        Ok(map)
    }
}
