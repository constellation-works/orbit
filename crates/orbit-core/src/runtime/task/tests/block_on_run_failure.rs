//! Terminal run publication precedes coupled-task cleanup at the injected publication seam.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::{TaskCreateParams, TaskReservationReleaseReason};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};
use tempfile::tempdir;

use crate::OrbitRuntime;

const PIPELINE_JOB: &str = "task_pr_pipeline";

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
    create_task(runtime, id_hint, None, Vec::new())
}

/// A backlog task, with `complexity` when admission needs one assessed.
fn create_task(
    runtime: &OrbitRuntime,
    id_hint: &str,
    complexity: Option<TaskComplexity>,
    context_files: Vec<String>,
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
            context_files,
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::Medium,
            complexity,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            crew_source: None,
            orchestrator: None,
            orchestrator_model: None,
            orchestrator_model_source: None,
            origin: None,
            comments: Vec::new(),
            context_creation: Vec::new(),
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

/// Deterministic interleaving: a task commit held by another participant delays
/// cleanup after the terminal run write. Run state alone is not completion of
/// the finalizer, which detached resume fixtures must wait for.
#[test]
fn terminal_run_publication_precedes_coupled_task_cleanup() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &terminal_run_publication_precedes_coupled_task_cleanup,
    )) {
        return;
    }
    let (_root, runtime, repo_root) = test_runtime();
    let runtime = std::sync::Arc::new(runtime);
    let task_id = create_backlog_task(&runtime, &repo_root, "cleanup-order");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    let mut finalizer = None;
    let mut at_terminal = None;
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(&task_id, &mut || {
            let worker_runtime = runtime.clone();
            let run_id = run.run_id.clone();
            finalizer = Some(std::thread::spawn(move || {
                finalize_failed(&worker_runtime, &run_id);
            }));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while runtime.get_job_run(&run.run_id)?.unwrap().state != JobRunState::Failed {
                assert!(
                    std::time::Instant::now() < deadline,
                    "run must terminalize while coupled-task cleanup waits for the task commit"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            at_terminal = Some(runtime.get_task(&task_id)?.status);
            Ok(())
        })
        .unwrap();
    // Release the task commit before joining, including on assertion failure.
    finalizer.unwrap().join().unwrap();
    assert_eq!(
        at_terminal,
        Some(TaskStatus::InProgress),
        "terminal publication alone does not establish coupled-task cleanup"
    );
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Blocked,
        "waiting for finalizer exit observes completed task cleanup"
    );
}
