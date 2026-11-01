//! [F2026-10-694] The stopped proof for a frozen-batch review task: only
//! the block its own coupled run's failure wrote proves the executor that
//! exited `incomplete_*` can no longer submit evidence, so settlement may
//! spend the bounded retry over the same frozen batch.

use chrono::{TimeZone, Utc};
use orbit_types::task::{TaskHistoryEntry, TaskStatus};

use super::super::task::blocked_by_failed_run;

fn entry(event: &str, to: Option<TaskStatus>, note: Option<String>) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc.with_ymd_and_hms(2026, 10, 29, 23, 9, 0).unwrap(),
        by: "system".into(),
        event: event.into(),
        note,
        from_status: None,
        to_status: to,
        operation_id: None,
        decision: None,
    }
}

fn failure_block(run_id: &str) -> TaskHistoryEntry {
    entry(
        "workflow_run_failed",
        Some(TaskStatus::Blocked),
        Some(format!(
            "workflow run failed: job=task_pr_pipeline, run_id={run_id}, \
             error_code=incomplete_examination, error=examination incomplete"
        )),
    )
}

#[test]
fn only_the_coupled_runs_failure_block_proves_the_action_stopped() {
    let started = entry("started", Some(TaskStatus::InProgress), None);

    // The incident's shape: the run failed early and blocked its task.
    assert!(blocked_by_failed_run(
        &[started.clone(), failure_block("run-1")],
        "run-1"
    ));

    // A block another run wrote proves nothing about this run's executor.
    assert!(!blocked_by_failed_run(
        &[started.clone(), failure_block("run-2")],
        "run-1"
    ));

    // An operator's block is not run terminalization.
    assert!(!blocked_by_failed_run(
        &[
            started.clone(),
            entry("blocked", Some(TaskStatus::Blocked), Some("waiting".into())),
        ],
        "run-1"
    ));

    // An interruption keeps its resume path; it is not a failed exit.
    assert!(!blocked_by_failed_run(
        &[
            started.clone(),
            entry(
                "workflow_run_interrupted",
                Some(TaskStatus::Blocked),
                Some("workflow run interrupted: job=x, run_id=run-1, error=-".into()),
            ),
        ],
        "run-1"
    ));

    // A later move supersedes the failure's block: a requeued or resumed
    // task may run again, so its evidence is replaceable once more.
    assert!(!blocked_by_failed_run(
        &[
            started,
            failure_block("run-1"),
            entry("requeued", Some(TaskStatus::Backlog), None),
        ],
        "run-1"
    ));
}
