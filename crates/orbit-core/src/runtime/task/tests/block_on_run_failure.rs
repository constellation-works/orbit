//! Sibling tests for `block_on_run_failure.rs`: a coupled task is moved
//! to `blocked` when its `task_pr_pipeline` run terminalizes as a failure or is
//! interrupted, the transition is idempotent, and it leaves `review`/`done`
//! tasks (and the workflow-admission allowlist) untouched. A run that failed
//! on a red base holds its task in the backlog instead.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT};
use orbit_store::{JobRunStepParams, TaskCreateParams, TaskReservationReleaseReason};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
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
            orchestrator: None,
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

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Orbit Test",
            "-c",
            "user.email=test@orbit.invalid",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_string()
}

/// [ORB-14258] A run whose required validation failed on its base exactly as
/// on the candidate holds its task in the backlog instead of blocking it. The
/// backlog snapshot withholds the task while the base still points at the red
/// commit and after it moves to another failing tip, then admits it only after
/// the command passes on a new base tip.
#[test]
fn a_red_base_failure_holds_the_task_until_the_command_passes() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::write(repo_root.join(".gitignore"), "/.orbit/\n").expect("ignore");
    std::fs::write(
        repo_root.join("Makefile"),
        "ci-lint:\n\t@echo lint is red >&2; exit 2\n",
    )
    .expect("write red-base command");
    git(&repo_root, &["init", "-q", "-b", "main"]);
    git(&repo_root, &["add", "-A"]);
    git(&repo_root, &["commit", "-q", "-m", "red base"]);
    let hold = orbit_types::workflow::BaselineRedHold {
        base_ref: "main".to_string(),
        base_sha: git(&repo_root, &["rev-parse", "HEAD"]),
        command: "make ci-lint".to_string(),
        run_id: String::new(),
    };
    let task_id = create_task(
        &runtime,
        "red-base",
        Some(TaskComplexity::Low),
        vec!["file:.gitignore".to_string()],
    );
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step_with_message(
        &runtime,
        &run.run_id,
        &hold.text("required validation 'make ci-lint' fails on the base too"),
    );

    assert!(finalize_failed(&runtime, &run.run_id));

    let task = runtime.get_task(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Backlog);
    assert!(
        failure_history_entries(&runtime, &task_id).is_empty(),
        "not blocked"
    );
    let latest = runtime
        .get_task_history(&task_id)
        .expect("history")
        .into_iter()
        .rev()
        .find(|entry| entry.to_status.is_some())
        .expect("a status decision");
    assert_eq!(latest.event, orbit_types::workflow::BASELINE_RED_HOLD_EVENT);
    let recorded = latest
        .note
        .as_deref()
        .and_then(orbit_types::workflow::BaselineRedHold::from_text)
        .expect("the note carries the hold");
    assert_eq!(recorded.run_id, run.run_id);
    assert_eq!(recorded.base_sha, hold.base_sha);

    let backlog = |runtime: &OrbitRuntime| {
        runtime
            .run_deterministic(
                "list_backlog_tasks",
                &serde_json::json!({}),
                &serde_json::json!({}),
                orbit_tools::ToolContext::default(),
            )
            .expect("list backlog tasks")
    };
    let held = backlog(&runtime);
    assert_eq!(held["task_ids"], serde_json::json!([]), "{held}");
    assert!(
        held["excluded"].as_array().is_some_and(|excluded| {
            excluded.iter().any(|entry| {
                entry["id"] == task_id.as_str() && entry["reason"] == "baseline_red_hold"
            })
        }),
        "{held}"
    );

    std::fs::write(
        repo_root.join("Makefile"),
        "ci-lint:\n\t@echo lint is still red >&2; exit 2\n",
    )
    .expect("write still-red base command");
    git(&repo_root, &["add", "Makefile"]);
    git(&repo_root, &["commit", "-q", "-m", "still red"]);
    let still_held = backlog(&runtime);
    assert_eq!(
        still_held["task_ids"],
        serde_json::json!([]),
        "{still_held}"
    );
    assert!(
        still_held["excluded"].as_array().is_some_and(|excluded| {
            excluded.iter().any(|entry| {
                entry["id"] == task_id.as_str() && entry["reason"] == "baseline_red_hold"
            })
        }),
        "a moved but still-red base keeps the task held: {still_held}"
    );

    std::fs::write(repo_root.join("Makefile"), "ci-lint:\n\t@echo lint-ok\n")
        .expect("write passing-base command");
    git(&repo_root, &["add", "Makefile"]);
    git(&repo_root, &["commit", "-q", "-m", "fix lint"]);
    let lifted = backlog(&runtime);
    assert_eq!(lifted["task_ids"], serde_json::json!([task_id]), "{lifted}");
}
