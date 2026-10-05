//! Sibling tests for `block_on_run_failure.rs`: a coupled task is moved
//! to `blocked` when its `task_pr_pipeline` run terminalizes as a failure or is
//! interrupted, the transition is idempotent, and it leaves `review`/`done`
//! tasks (and the workflow-admission allowlist) untouched.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT};
use orbit_store::{JobRunStepParams, TaskCreateParams, TaskReservationReleaseReason};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType};
use tempfile::tempdir;

use crate::OrbitRuntime;

const PIPELINE_JOB: &str = "task_pr_pipeline";
const FAILING_STEP_MESSAGE: &str = "step `implement_one` completed with success=false";

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn create_backlog_task(
    runtime: &OrbitRuntime,
    _repo_root: &std::path::Path,
    id_hint: &str,
) -> String {
    runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: format!("task {id_hint}"),
            description: "test".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: Vec::new(),
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task")
        .id
}

/// Mirror `worktree_setup`'s coupling-in: stamp the run's `job_run_id` and move
/// the task to `status`.
fn couple_task(runtime: &OrbitRuntime, task_id: &str, run_id: &str, status: TaskStatus) {
    runtime
        .apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                status: Some(status),
                job_run_id: Some(run_id.to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("couple task to run");
}

fn insert_running_pipeline_run(runtime: &OrbitRuntime) -> JobRun {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(PIPELINE_JOB, 1, Utc::now(), None, None)
        .expect("insert pipeline run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("mark run running");
    run
}

/// Record the single job-level diagnostic step the pipeline emits when
/// `implement_one` fails, so the block note has real error context to surface.
fn record_failing_step(runtime: &OrbitRuntime, run_id: &str) {
    record_failing_step_with_message(runtime, run_id, FAILING_STEP_MESSAGE);
}

fn record_failing_step_with_message(runtime: &OrbitRuntime, run_id: &str, message: &str) {
    let now = Utc::now();
    runtime
        .stores()
        .jobs()
        .complete_job_run_step(
            run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "agent_implement".to_string(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".to_string()),
                error_message: Some(message.to_string()),
            },
        )
        .expect("record failing step");
}

fn finalize_failed(runtime: &OrbitRuntime, run_id: &str) -> bool {
    runtime
        .finalize_job_run_with_reservation_cleanup(
            run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize failed run")
}

fn failure_history_entries(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Vec<orbit_types::task::TaskHistoryEntry> {
    runtime
        .get_task_history(task_id)
        .expect("task history")
        .into_iter()
        .filter(|entry| entry.event == WORKFLOW_RUN_FAILED_EVENT)
        .collect()
}

#[test]
fn re_running_terminalization_is_idempotent_and_respects_human_recovery() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "idempotent");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step(&runtime, &run.run_id);

    assert!(finalize_failed(&runtime, &run.run_id));
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );

    // A human/orchestrator moves the task on (here: back to backlog for a
    // re-plan). Only the winning terminal transition may block it.
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::Backlog);

    // Re-running terminalization on the already-terminal run does not re-fire
    // the block: it neither re-blocks the task a human moved on nor duplicates
    // history. The compatibility bool still reports an existing run on replay;
    // the atomic store outcome is the guard for the side effect.
    finalize_failed(&runtime, &run.run_id);
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Backlog
    );
    assert_eq!(
        failure_history_entries(&runtime, &task_id).len(),
        1,
        "terminalization must not duplicate the failure event"
    );
}

/// [ORB-11305] A withdrawal that lands while the run is being torn down must
/// survive the teardown.
///
/// The ordering this pins comes straight from the incident: the human withdrew
/// the task, the orchestrator then cancelled the run that had started it
/// anyway, and cleanup ran *after* the withdrawal. Blocking the task there
/// would replace the owner's newer decision with `blocked`, so the withdrawal
/// has to be re-applied by hand once the run finishes unwinding.
#[test]
fn failure_cleanup_leaves_a_newer_human_withdrawal_alone() {
    for withdrawn_to in [
        TaskStatus::Proposed,
        TaskStatus::Someday,
        TaskStatus::Archived,
        TaskStatus::Rejected,
    ] {
        let (_root, runtime, repo_root) = test_runtime();
        let task_id = create_backlog_task(&runtime, &repo_root, "withdrawn");
        let run = insert_running_pipeline_run(&runtime);
        couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);

        // The human withdraws while the run is still live.
        couple_task(&runtime, &task_id, &run.run_id, withdrawn_to);

        // Only then does the cancelled run's cleanup arrive.
        record_failing_step(&runtime, &run.run_id);
        finalize_failed(&runtime, &run.run_id);

        assert_eq!(
            runtime.get_task(&task_id).expect("task").status,
            withdrawn_to,
            "{withdrawn_to} is newer than the run's failure and must win"
        );
        assert!(
            failure_history_entries(&runtime, &task_id).is_empty(),
            "{withdrawn_to} must not be annotated as a workflow failure"
        );
    }
}
