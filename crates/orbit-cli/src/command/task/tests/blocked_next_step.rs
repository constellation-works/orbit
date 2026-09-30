//! Sibling tests for `task/blocked_next_step.rs` (docs/design-patterns/test_layout.md).

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_types::task::{Task, TaskHistoryEntry};
use orbit_types::workflow::{JobRun, JobRunState};

use crate::command::task::blocked_next_step::{blocked_next_step, next_step, run_id_in_note};

fn blocked_entry(event: &str, note: &str) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "orbit".to_string(),
        event: event.to_string(),
        note: Some(note.to_string()),
        from_status: Some(TaskStatus::InProgress),
        to_status: Some(TaskStatus::Blocked),
    }
}

fn blocked_task(runtime: &OrbitRuntime) -> Task {
    let mut task = runtime
        .add_task(TaskAddParams {
            title: "Blocked by a failed run".to_string(),
            ..TaskAddParams::default()
        })
        .expect("add task");
    task.status = TaskStatus::Blocked;
    task
}

fn seed_run(runtime: &OrbitRuntime, run_id: &str, state: JobRunState) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let store = runtime.sqlite_store().expect("store");
    let now = Utc::now();
    let run = JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: "task_auto_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    store
        .upsert_job_run_for_workspace(&workspace_id, &run, None)
        .expect("insert run");
}

#[test]
fn the_run_id_is_read_from_the_failure_note() {
    let note =
        "workflow run failed: job=task_auto_pipeline, run_id=jrun-abc-c1, error_code=x, error=boom";
    assert_eq!(run_id_in_note(note), Some("jrun-abc-c1"));
    assert_eq!(run_id_in_note("blocked by hand"), None);
}

#[test]
fn a_failed_run_names_its_resume_command_and_the_backlog_move() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let task = blocked_task(&runtime);
    seed_run(&runtime, "jrun-failed-leaf", JobRunState::Failed);
    let history = [blocked_entry(
        "workflow_run_failed",
        "workflow run failed: job=task_auto_pipeline, run_id=jrun-failed-leaf, error_code=-, error=boom",
    )];

    let step = blocked_next_step(&runtime, &task, &history).expect("guidance for a blocked task");

    assert_eq!(step.run_id, "jrun-failed-leaf");
    assert_eq!(
        step.resume_command.as_deref(),
        Some("orbit job resume jrun-failed-leaf")
    );
    assert_eq!(
        step.requeue_command,
        format!("orbit task update {} --status backlog", task.id)
    );
    assert!(step.line().contains("orbit job resume jrun-failed-leaf"));
    assert_eq!(step.to_json()["run_id"], "jrun-failed-leaf");
}

#[test]
fn a_cancelled_run_cannot_be_resumed_so_only_the_backlog_move_is_offered() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let task = blocked_task(&runtime);
    seed_run(&runtime, "jrun-cancelled-leaf", JobRunState::Cancelled);
    let history = [blocked_entry(
        "workflow_run_failed",
        "workflow run failed: job=task_auto_pipeline, run_id=jrun-cancelled-leaf, error_code=-, error=-",
    )];

    let step = blocked_next_step(&runtime, &task, &history).expect("guidance");

    assert_eq!(step.resume_command, None);
    assert!(step.line().contains("--status backlog"));
    assert!(!step.line().contains("job resume"));
}

#[test]
fn a_block_no_run_caused_and_a_task_that_is_not_blocked_get_no_guidance() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let task = blocked_task(&runtime);
    let by_hand = [blocked_entry("status_changed", "waiting on a decision")];
    assert!(blocked_next_step(&runtime, &task, &by_hand).is_none());

    let mut open = task.clone();
    open.status = TaskStatus::Backlog;
    let history = [blocked_entry(
        "workflow_run_failed",
        "workflow run failed: run_id=jrun-old, error=x",
    )];
    assert!(blocked_next_step(&runtime, &open, &history).is_none());
}

#[test]
fn an_unreadable_run_still_offers_the_backlog_move() {
    let step = next_step("ORB-1", "jrun-gone".to_string(), false);
    assert_eq!(step.resume_command, None);
    assert_eq!(
        step.requeue_command,
        "orbit task update ORB-1 --status backlog"
    );
}
