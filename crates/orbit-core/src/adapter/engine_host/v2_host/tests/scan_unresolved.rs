//! Unit coverage for `scan_unresolved_work` [ORB-10779].

use chrono::Utc;
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_store::{SessionLogAppendParams, SessionLogEntry, SessionLogKind, SessionLogStore};
use orbit_tools::ToolContext;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::TaskAddParams;

fn scan(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "scan_unresolved_work",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("scan unresolved work")
}

fn scan_err(runtime: &OrbitRuntime, input: Value) -> DispatchError {
    runtime
        .run_deterministic(
            "scan_unresolved_work",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect_err("scan should fail")
}

fn seed_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task")
        .id
}

fn seed_run(runtime: &OrbitRuntime, job_id: &str, state: JobRunState) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now(), None, None)
        .expect("insert job run");
    if state == JobRunState::Pending {
        return run.run_id;
    }
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    if state == JobRunState::Running {
        return run.run_id;
    }
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, state, Utc::now(), Some(1))
        .expect("finalize job run");
    run.run_id
}

fn seed_session_log(runtime: &OrbitRuntime, kind: SessionLogKind, body: &str) -> SessionLogEntry {
    SessionLogStore::new(runtime.paths().orbit_dir.clone())
        .append(SessionLogAppendParams {
            kind,
            body: body.to_string(),
            related_task_ids: Vec::new(),
            related_run_ids: Vec::new(),
        })
        .expect("seed session log")
}

fn string_ids(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("id array")
        .iter()
        .map(|id| id.as_str().expect("id string").to_string())
        .collect()
}

#[test]
fn empty_workspace_is_a_successful_noop() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(true));
    assert_eq!(output["task_ids"], json!([]));
    assert_eq!(output["run_ids"], json!([]));
    assert_eq!(output["check_later_ids"], json!([]));
    assert_eq!(output["task_count"], json!(0));
    assert_eq!(output["run_count"], json!(0));
    assert_eq!(output["check_later_count"], json!(0));
}

#[test]
fn includes_proposed_backlog_and_blocked_tasks() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let proposed = seed_task(&runtime, "proposed", TaskStatus::Proposed);
    let backlog = seed_task(&runtime, "backlog", TaskStatus::Backlog);
    let blocked = seed_task(&runtime, "blocked", TaskStatus::Blocked);

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(false));
    let mut ids = string_ids(&output["task_ids"]);
    ids.sort();
    let mut expected = vec![proposed, backlog, blocked];
    expected.sort();
    assert_eq!(ids, expected);
}

#[test]
fn excludes_in_progress_and_review_tasks() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    seed_task(&runtime, "live", TaskStatus::InProgress);
    seed_task(&runtime, "waiting-human", TaskStatus::Review);
    seed_task(&runtime, "done", TaskStatus::Done);
    seed_task(&runtime, "rejected", TaskStatus::Rejected);
    seed_task(&runtime, "someday", TaskStatus::Someday);

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(true));
    assert_eq!(output["task_ids"], json!([]));
}

#[test]
fn includes_failed_and_timeout_runs() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let failed = seed_run(&runtime, "task_pr_pipeline", JobRunState::Failed);
    let timed_out = seed_run(&runtime, "task_auto_pipeline", JobRunState::Timeout);

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(false));
    let mut ids = string_ids(&output["run_ids"]);
    ids.sort();
    let mut expected = vec![failed, timed_out];
    expected.sort();
    assert_eq!(ids, expected);
}

#[test]
fn excludes_cancelled_and_live_runs() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    seed_run(&runtime, "task_pr_pipeline", JobRunState::Cancelled);
    seed_run(&runtime, "task_pr_pipeline", JobRunState::Pending);
    seed_run(&runtime, "task_pr_pipeline", JobRunState::Running);
    seed_run(&runtime, "task_pr_pipeline", JobRunState::Success);
    seed_run(&runtime, "task_pr_pipeline", JobRunState::Interrupted);

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(true));
    assert_eq!(output["run_ids"], json!([]));
}

#[test]
fn excludes_epic_pipeline_own_failed_runs() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    seed_run(&runtime, "epic_pipeline", JobRunState::Failed);
    seed_run(&runtime, "epic_pipeline", JobRunState::Timeout);
    let child = seed_run(&runtime, "task_gate_pipeline", JobRunState::Failed);

    let output = scan(&runtime, json!({}));

    assert_eq!(output["run_ids"], json!([child]));
}

#[test]
fn unresolved_check_later_wakes_the_scan() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    seed_session_log(
        &runtime,
        SessionLogKind::Status,
        "previous fire drained nothing",
    );
    seed_session_log(&runtime, SessionLogKind::Note, "not a wake reason");
    let later = seed_session_log(&runtime, SessionLogKind::CheckLater, "recheck CI");

    let output = scan(&runtime, json!({}));

    assert_eq!(output["empty"], json!(false));
    assert_eq!(output["check_later_ids"], json!([later.id]));
}

#[test]
fn fail_if_nonempty_is_a_noop_when_empty() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();

    let output = scan(&runtime, json!({ "fail_if_nonempty": true }));

    assert_eq!(output["empty"], json!(true));
}

#[test]
fn fail_if_nonempty_fails_closed_on_leftover_work() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let task_id = seed_task(&runtime, "leftover", TaskStatus::Backlog);

    let error = scan_err(&runtime, json!({ "fail_if_nonempty": true }));
    match error {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "scan_unresolved_work");
            assert!(message.contains(&task_id), "{message}");
            assert!(message.contains("unresolved work remains"), "{message}");
        }
        other => panic!("expected leftover-scan failure, got {other:?}"),
    }
}
