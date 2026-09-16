//! Deterministic unresolved-work scan [ORB-10779 / ORB-10818].
//!
//! Read-only workspace drain predicate. Wakes on `proposed` / `backlog` /
//! `blocked` tasks, `failed` / `timeout` job-runs (except `epic_pipeline`
//! itself), and unresolved `check_later` session-log entries. Empty is
//! success, not an error. This is no longer `epic_pipeline`'s completion
//! gate — that job uses `list_epic_descendants`.

use orbit_engine::DispatchError;
use orbit_store::compose::workspace_session_log_store;
use orbit_store::contracts::{JobRunQuery, SessionLogFilter, SessionLogKind};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// Job whose own failed/timeout rows must not re-admit the drain loop.
pub(super) const EPIC_PIPELINE_JOB_ID: &str = "epic_pipeline";

const WAKE_TASK_STATUSES: [TaskStatus; 3] = [
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Blocked,
];

const WAKE_RUN_STATES: [JobRunState; 2] = [JobRunState::Failed, JobRunState::Timeout];

pub(super) fn scan_unresolved_work(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let fail_if_nonempty = input
        .get("fail_if_nonempty")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut task_ids: Vec<String> = runtime
        .stores()
        .tasks()
        .list_tasks()
        .map_err(|err| action_failed(action, format!("list tasks: {err}")))?
        .into_iter()
        .filter(|task| WAKE_TASK_STATUSES.contains(&task.status))
        .map(|task| task.id)
        .collect();
    task_ids.sort();

    let mut run_ids: Vec<String> = Vec::new();
    for state in WAKE_RUN_STATES {
        run_ids.extend(
            runtime
                .stores()
                .jobs()
                .list_job_runs_filtered(&JobRunQuery {
                    state: Some(state),
                    include_steps: false,
                    ..JobRunQuery::default()
                })
                .map_err(|err| action_failed(action, format!("list job runs: {err}")))?
                .into_iter()
                .filter(|run| run.job_id != EPIC_PIPELINE_JOB_ID)
                .map(|run| run.run_id),
        );
    }
    run_ids.sort();

    let mut check_later_ids: Vec<String> =
        workspace_session_log_store(runtime.paths().orbit_dir.clone())
            .list(SessionLogFilter {
                kind: Some(SessionLogKind::CheckLater),
                unresolved_only: true,
                ..SessionLogFilter::default()
            })
            .map_err(|err| action_failed(action, format!("list session log: {err}")))?
            .into_iter()
            .map(|entry| entry.id)
            .collect();
    check_later_ids.sort();

    let empty = task_ids.is_empty() && run_ids.is_empty() && check_later_ids.is_empty();
    if fail_if_nonempty && !empty {
        return Err(action_failed(
            action,
            format!(
                "unresolved work remains after drain: tasks=[{}], runs=[{}], check_later=[{}]",
                task_ids.join(", "),
                run_ids.join(", "),
                check_later_ids.join(", ")
            ),
        ));
    }

    Ok(json!({
        "empty": empty,
        "task_ids": task_ids,
        "run_ids": run_ids,
        "check_later_ids": check_later_ids,
        "task_count": task_ids.len(),
        "run_count": run_ids.len(),
        "check_later_count": check_later_ids.len(),
    }))
}

fn action_failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}
