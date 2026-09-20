//! Sibling tests for `block_on_run_failure.rs`: a coupled task is moved
//! to `blocked` when its `task_pr_pipeline` run terminalizes as a failure, the
//! transition is idempotent, and it leaves `review`/`done` tasks (and the
//! workflow-admission allowlist) untouched.

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
fn failed_pipeline_run_blocks_coupled_in_progress_task() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "coupled");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step(&runtime, &run.run_id);

    assert!(finalize_failed(&runtime, &run.run_id));

    let task = runtime.get_task(&task_id).expect("task after failure");
    assert_eq!(task.status, TaskStatus::Blocked);
    // Stays coupled to the failed run so the failure is traceable.
    assert_eq!(task.job_run_id.as_deref(), Some(run.run_id.as_str()));

    let entries = failure_history_entries(&runtime, &task_id);
    assert_eq!(entries.len(), 1, "exactly one workflow-run-failed event");
    let note = entries[0].note.as_deref().unwrap_or_default();
    assert!(
        note.contains(&run.run_id),
        "note names the failed run: {note}"
    );
    assert!(
        note.contains("implement_one"),
        "note names the failing step: {note}"
    );
    assert_eq!(entries[0].to_status, Some(TaskStatus::Blocked));
}

/// [ORB-10879] A sandbox write denial must be recoverable from the task record
/// alone. ORB-10878 reached its operator as the model's own narrative ("remount
/// the filesystem") with an empty `artifacts` array and no comments, so the one
/// piece of text that names the attempted path and the shadowing rule has to
/// survive run terminalization into the task's history — which is what
/// `orbit task show` renders.
#[test]
fn sandbox_write_denial_is_recoverable_from_the_blocked_task_record() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "denied-write");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);

    // The real attribution text, produced by the helper the runner uses — not a
    // re-spelling of it, so a change to that format fails this test too.
    let worktree = std::path::Path::new("/tmp/orbit-jrun-doc-duties");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "unrestricted".to_string(),
        read: vec![format!("{}/**", worktree.display())],
        modify: vec![
            format!("{}/**", worktree.display()),
            format!("!{}/.orbit/**", worktree.display()),
        ],
    };
    let denied = worktree.join(".orbit/state/orbit.db");
    let diagnostic = orbit_exec::linux_bwrap_write_grant_diagnostic(&profile, &denied)
        .expect("grant diagnostic")
        .expect("the path is denied by this profile");
    record_failing_step_with_message(
        &runtime,
        &run.run_id,
        &format!("{FAILING_STEP_MESSAGE} {diagnostic}"),
    );

    assert!(finalize_failed(&runtime, &run.run_id));

    let task = runtime.get_task(&task_id).expect("task after failure");
    assert_eq!(task.status, TaskStatus::Blocked);

    let entries = failure_history_entries(&runtime, &task_id);
    assert_eq!(entries.len(), 1, "exactly one workflow-run-failed event");
    let note = entries[0].note.as_deref().unwrap_or_default();
    assert!(
        note.contains(&denied.display().to_string()),
        "note must name the attempted path: {note}"
    );
    assert!(
        note.contains("denyModify rule"),
        "note must name the shadowing rule: {note}"
    );
    assert!(
        note.contains(&format!("!{}/.orbit/**", worktree.display())),
        "note must quote the exact deny: {note}"
    );
}

#[test]
fn failed_pipeline_run_leaves_review_and_done_tasks_untouched() {
    let (_root, runtime, repo_root) = test_runtime();
    let review_id = create_backlog_task(&runtime, &repo_root, "review");
    let done_id = create_backlog_task(&runtime, &repo_root, "done");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &review_id, &run.run_id, TaskStatus::Review);
    couple_task(&runtime, &done_id, &run.run_id, TaskStatus::Done);
    record_failing_step(&runtime, &run.run_id);

    finalize_failed(&runtime, &run.run_id);

    assert_eq!(
        runtime.get_task(&review_id).expect("review task").status,
        TaskStatus::Review
    );
    assert_eq!(
        runtime.get_task(&done_id).expect("done task").status,
        TaskStatus::Done
    );
    assert!(failure_history_entries(&runtime, &review_id).is_empty());
    assert!(failure_history_entries(&runtime, &done_id).is_empty());
}

#[test]
fn failed_pipeline_run_blocks_every_task_in_bundle() {
    let (_root, runtime, repo_root) = test_runtime();
    let ids: Vec<String> = (0..3)
        .map(|i| create_backlog_task(&runtime, &repo_root, &format!("bundle-{i}")))
        .collect();
    let run = insert_running_pipeline_run(&runtime);
    for id in &ids {
        couple_task(&runtime, id, &run.run_id, TaskStatus::InProgress);
    }
    record_failing_step(&runtime, &run.run_id);

    finalize_failed(&runtime, &run.run_id);

    for id in &ids {
        assert_eq!(
            runtime.get_task(id).expect("bundle task").status,
            TaskStatus::Blocked,
            "task {id} coupled to the failed run must be blocked"
        );
    }
}

#[test]
fn cancelled_pipeline_run_blocks_coupled_task() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "cancelled");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Cancelled,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize cancelled run");

    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );
}

#[test]
fn interrupted_pipeline_run_leaves_coupled_task_in_progress() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "interrupted");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);

    // Interrupted runs are resumable from checkpoints — the task must stay
    // `in_progress` for the resume, not blocked.
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Interrupted,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::StaleRunReconciled,
        )
        .expect("finalize interrupted run");

    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::InProgress
    );
}

#[test]
fn successful_pipeline_run_does_not_block_coupled_task() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "success");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize success run");

    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::InProgress
    );
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
    // re-plan). The `changed` gate on terminalization is what protects this.
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::Backlog);

    // Re-running terminalization on the already-terminal run does not re-fire
    // the block: it neither re-blocks the task a human moved on nor duplicates
    // history. (The store reports the finalize write as applied even on replay,
    // so the guard is the pre-finalize terminal check, not this return value.)
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

#[test]
fn blocked_task_is_rejected_by_workflow_admission() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "gated");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step(&runtime, &run.run_id);
    finalize_failed(&runtime, &run.run_id);
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );

    // Backlog discovery / the ship sweep only pick up admittable statuses;
    // a blocked task is skipped because admission rejects it.
    runtime
        .ensure_task_can_enter_workflow_as_system(&task_id, "worktree_setup")
        .expect_err("blocked task must not be admissible into a workflow");
    runtime
        .admit_task_for_workflow_as_system(&task_id, "worktree_setup")
        .expect_err("system admission must reject a blocked task");

    // A blocked task is not returned by backlog discovery (it lists `Backlog`).
    let backlog = runtime
        .list_tasks_filtered(Some(TaskStatus::Backlog), None, None, None, None, None)
        .expect("list backlog");
    assert!(
        !backlog.iter().any(|task| task.id == task_id),
        "blocked task must not appear in backlog discovery"
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

/// The counterpart: a task still executing under the failed run is exactly what
/// this cleanup is for, and must still be blocked. Narrowing the transition
/// must not turn it off.
#[test]
fn failure_cleanup_still_blocks_a_task_left_in_progress() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "stranded");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step(&runtime, &run.run_id);
    finalize_failed(&runtime, &run.run_id);

    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );
    assert_eq!(failure_history_entries(&runtime, &task_id).len(), 1);
}

/// [ORB-11305] The documented way out of `blocked` still works, and lands the
/// task somewhere workflow admission accepts — otherwise narrowing admission
/// would have made every blocked task unrecoverable.
#[test]
fn an_explicitly_restarted_blocked_task_is_admissible_again() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = create_backlog_task(&runtime, &repo_root, "recovered");
    let run = insert_running_pipeline_run(&runtime);
    couple_task(&runtime, &task_id, &run.run_id, TaskStatus::InProgress);
    record_failing_step(&runtime, &run.run_id);
    finalize_failed(&runtime, &run.run_id);
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Blocked
    );

    // `start_task` keeps its own plan prerequisite; that gate is unrelated to
    // admission and stays exactly where it was.
    runtime
        .update_task(
            &task_id,
            crate::application::task::TaskUpdateParams {
                plan: Some("1. Retry the failed step.".to_string()),
                ..Default::default()
            },
        )
        .expect("author a plan");
    let restarted = runtime
        .start_task(&task_id, None, None)
        .expect("a human may restart a blocked task");
    assert_eq!(restarted.status, TaskStatus::InProgress);
    let admitted = runtime
        .ensure_task_can_enter_workflow_as_system(&task_id, "worktree_setup")
        .expect("an explicitly restarted task is admissible");
    assert_eq!(admitted.status, TaskStatus::InProgress);
}
