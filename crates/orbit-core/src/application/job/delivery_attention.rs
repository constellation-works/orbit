//! Delivery attention for `review` (and blocked-with-PR) tasks, derived
//! only from durable run, history and ledger state. No forge reads.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    JobRun, JobRunState, PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixAttempt,
    PrAutoFixPush, REVIEW_ABANDONED_MARKER,
};
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::application::job::{run_display_steps, run_error_step};
use crate::application::task::TaskRow;
use crate::runtime::run_input::carried_task_ids;

#[cfg(test)]
mod tests;

const DELIVERY_JOB: &str = "task_pr_pipeline";
const CLAIMED_DELIVERY_JOB: &str = "task_claimed_pr_pipeline";
/// YAML step whose output and error carry the landing-review gate settlement.
pub const LANDING_REVIEW_SETTLE_STEP: &str = "landing_review_gate_settle";

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

fn single_task(run: &JobRun, task_id: &str) -> bool {
    let carried = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
    carried.len() == 1 && carried.contains(task_id)
}

/// Verdict and finding count recorded by the landing-review gate step.
///
/// `output` is that step's pipeline output. `step_error` is the step's own
/// error text, which is where a blocked settlement records the verdict and
/// the finding count. A known gate word (`awaiting_evidence`, and the other
/// settlements that are not a review verdict) is the displayed outcome when
/// the step recorded no verdict. The count stays absent when the step did
/// not record one; callers tell that from a missing verdict.
pub fn landing_review_gate_facts(output: Option<&Value>, step_error: Option<&str>) -> Value {
    let object = output.and_then(Value::as_object);
    let mut verdict = text_field(object, "verdict").map(str::to_string);
    let gate = text_field(object, "gate");
    let mut count = object.and_then(|object| finding_count(object.get("findings")));
    if let Some(message) = step_error.map(str::trim).filter(|text| !text.is_empty()) {
        if verdict.is_none() {
            verdict = verdict_from_message(message).map(str::to_string);
        }
        if count.is_none() {
            count = finding_count_from_message(message);
        }
    }
    let Some(shown) = verdict.or_else(|| gate.and_then(known_gate_label).map(str::to_string))
    else {
        return json!({ "availability": "unavailable" });
    };
    json!({
        "availability": "read",
        "verdict": shown,
        "finding_count": count,
    })
}

fn text_field<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a str> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn finding_count(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if let Some(count) = value.as_u64() {
        return Some(count);
    }
    if let Some(count) = value.as_i64().filter(|count| *count >= 0) {
        return Some(count as u64);
    }
    value.as_array().map(|items| items.len() as u64)
}

fn verdict_from_message(message: &str) -> Option<&str> {
    // An abandoned settlement records no verdict: the step error says the
    // attempt "was released without a verdict". Report the gate outcome
    // instead of misreading the parenthetical "(verdict incomplete, ...)".
    if message.contains(REVIEW_ABANDONED_MARKER) {
        return Some("abandoned");
    }
    let rest = message.split("verdict ").nth(1)?;
    let token = rest
        .split([' ', '('])
        .next()?
        .trim()
        .trim_matches([',', ';', ':', '.', '!', '?', '"', '\'']);
    (!token.is_empty()).then_some(token)
}

fn finding_count_from_message(message: &str) -> Option<u64> {
    let marker = message
        .find(" finding(s) recorded")
        .or_else(|| message.find(" finding recorded"))?;
    message[..marker]
        .split_whitespace()
        .next_back()?
        .parse()
        .ok()
}

fn known_gate_label(gate: &str) -> Option<&'static str> {
    match gate {
        "awaiting_evidence" => Some("awaiting evidence"),
        "forge_unavailable" => Some("forge unavailable"),
        "not_required" => Some("not required"),
        "abandoned" => Some("abandoned"),
        "passed" => Some("passed"),
        _ => None,
    }
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
            let newest = runs.iter().find(|run| single_task(run, &row.task.id));
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

    /// The newest single-task delivery run for `task_id`.
    ///
    /// Owner (`task_pr_pipeline`) and claimed (`task_claimed_pr_pipeline`)
    /// pipelines both count. Each contributes its newest run that carries
    /// exactly this task, and the later of the two wins. An equal timestamp
    /// keeps the owner pipeline, which is read first. This does not change
    /// which runs [`Self::delivery_attention_map`] treats as delivery attention.
    pub fn newest_single_task_delivery_run(
        &self,
        task_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let ids = [task_id.to_string()];
        let mut best: Option<JobRun> = None;
        for job in [DELIVERY_JOB, CLAIMED_DELIVERY_JOB] {
            let runs = self
                .stores()
                .jobs()
                .recent_job_runs_for_tasks(job, &ids, 8)?;
            let Some(runs) = runs.get(task_id) else {
                continue;
            };
            let Some(newest) = runs.iter().find(|run| single_task(run, task_id)) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|current| run_at(newest) > run_at(current))
            {
                best = Some(newest.clone());
            }
        }
        Ok(best)
    }
}
