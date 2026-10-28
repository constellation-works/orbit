use chrono::{DateTime, Utc};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    JobRun, PR_AUTO_FIX_ADMITTED_EVENT, PR_AUTO_FIX_PUSHED_EVENT, PrAutoFixAttempt, PrAutoFixPush,
};

use crate::runtime::run_input::carried_task_ids;

pub(super) fn eligible(task: &Task) -> bool {
    task.status == TaskStatus::Review
        || (task.status == TaskStatus::Blocked && task.github_pr_number().is_some())
}

pub(super) fn latest_attempt(
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

pub(super) fn latest_push_at(
    history: &[TaskHistoryEntry],
    pr_number: Option<&str>,
) -> Option<DateTime<Utc>> {
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

pub(super) fn run_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

pub(super) fn single_task(run: &JobRun, task_id: &str) -> bool {
    let carried = run.input.as_ref().map(carried_task_ids).unwrap_or_default();
    carried.len() == 1 && carried.contains(task_id)
}
