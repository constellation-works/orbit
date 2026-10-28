use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use super::history;

/// Pure projection from already-read durable state.
pub fn delivery_attention_value(
    task: &Task,
    history: &[TaskHistoryEntry],
    newest_run: Option<&JobRun>,
    failed_step_id: Option<&str>,
    repairing: bool,
) -> Option<Value> {
    if !history::eligible(task) {
        return None;
    }
    let pr_number = task.github_pr_number().map(str::to_string);
    let attempt = history::latest_attempt(history, pr_number.as_deref());
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
        let ledger_at = history::latest_push_at(history, pr_number.as_deref())
            .map_or(*admitted_at, |pushed_at| pushed_at.max(*admitted_at));
        if history::run_at(run) > ledger_at {
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
