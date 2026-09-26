//! Human task transitions retain the status they read when they write.

use chrono::Utc;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};

use super::test_runtime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn add_task(runtime: &crate::OrbitRuntime, title: &str) -> orbit_types::task::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise transition compare-and-set behavior.".to_string(),
            acceptance_criteria: vec![
                "The transition must not overwrite a newer status.".to_string(),
            ],
            plan: "Use the transition lock.".to_string(),
            ..Default::default()
        })
        .expect("add task")
}

fn set_status(runtime: &crate::OrbitRuntime, id: &str, status: TaskStatus) {
    runtime
        .update_task(
            id,
            TaskUpdateParams {
                status: Some(status),
                ..Default::default()
            },
        )
        .expect("set status through the store");
}

fn with_changed_status(runtime: &crate::OrbitRuntime, id: &str, test: impl FnOnce()) {
    runtime.set_transition_read_hook_status(Some(id), Some(TaskStatus::Rejected));
    test();
    runtime.set_transition_read_hook_status(None, None);
}

#[test]
fn approve_does_not_overwrite_a_status_changed_after_its_read() {
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Approve CAS");
    set_status(&runtime, &task.id, TaskStatus::Review);

    with_changed_status(&runtime, &task.id, || {
        let error = runtime
            .approve_task(&task.id, None, None)
            .expect_err("approve must lose to the direct store update");
        assert!(
            error.to_string().contains("status changed to 'rejected'"),
            "{error}"
        );
    });

    assert_eq!(
        runtime.get_task(&task.id).expect("current task").status,
        TaskStatus::Rejected
    );
}

#[test]
fn rejected_review_approval_does_not_apply_resolves_side_effects() {
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Approve resolves CAS");
    let frictions = crate::runtime::friction::store_for(&runtime).expect("friction store");
    let friction = frictions
        .add(FrictionAddParams {
            model: "codex".to_string(),
            title: Some("Approval race".to_string()),
            body: "The direct status change must win.".to_string(),
            tags: Vec::new(),
            during_task: None,
            created_at: Utc::now(),
        })
        .expect("add friction");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                relations: Some(vec![TaskRelation {
                    relation_type: TaskRelationType::Resolves,
                    target: friction.record.id.clone(),
                }]),
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("prepare review task with resolves relation");

    with_changed_status(&runtime, &task.id, || {
        runtime
            .approve_task(&task.id, None, None)
            .expect_err("approval must lose to the direct rejection");
    });

    assert_eq!(
        frictions
            .show(&friction.record.id)
            .expect("show friction")
            .expect("friction remains")
            .record
            .status,
        FrictionStatus::Open
    );
}

#[test]
fn start_does_not_overwrite_a_status_changed_after_its_read() {
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Start CAS");
    set_status(&runtime, &task.id, TaskStatus::Backlog);

    with_changed_status(&runtime, &task.id, || {
        let error = runtime
            .start_task(&task.id, None, None)
            .expect_err("start must lose to the direct store update");
        assert!(
            error.to_string().contains("status changed to 'rejected'"),
            "{error}"
        );
    });

    assert_eq!(
        runtime.get_task(&task.id).expect("current task").status,
        TaskStatus::Rejected
    );
}

#[test]
fn reject_does_not_overwrite_a_status_changed_after_its_read() {
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Reject CAS");
    set_status(&runtime, &task.id, TaskStatus::Review);

    with_changed_status(&runtime, &task.id, || {
        let error = runtime
            .reject_task(&task.id, "not ready".to_string(), None)
            .expect_err("reject must lose to the direct store update");
        assert!(
            error.to_string().contains("status changed to 'rejected'"),
            "{error}"
        );
    });

    assert_eq!(
        runtime.get_task(&task.id).expect("current task").status,
        TaskStatus::Rejected
    );
}

#[test]
fn transition_read_hook_does_not_affect_a_sibling_runtime_with_the_same_task_id() {
    let (_root_a, runtime_a) = test_runtime();
    let (_root_b, runtime_b) = test_runtime();
    let task_a = add_task(&runtime_a, "Hooked CAS");
    let task_b = add_task(&runtime_b, "Unrelated start");
    assert_eq!(
        task_a.id, task_b.id,
        "both empty stores mint the same first task id"
    );
    set_status(&runtime_a, &task_a.id, TaskStatus::Backlog);
    set_status(&runtime_b, &task_b.id, TaskStatus::Backlog);

    runtime_a.set_transition_read_hook_status(Some(&task_a.id), Some(TaskStatus::Rejected));

    runtime_b
        .start_task(&task_b.id, None, None)
        .expect("sibling runtime must not observe another instance's read hook");
    assert_eq!(
        runtime_b.get_task(&task_b.id).expect("current task").status,
        TaskStatus::InProgress
    );

    let error = runtime_a
        .start_task(&task_a.id, None, None)
        .expect_err("armed runtime must still lose to its own hook");
    assert!(
        error.to_string().contains("status changed to 'rejected'"),
        "{error}"
    );
    runtime_a.set_transition_read_hook_status(None, None);
}

fn block_by_run_failure(runtime: &crate::OrbitRuntime, id: &str, run_id: &str, error: &str) {
    use orbit_engine::RuntimeHost;
    // Coupled to its run as `worktree_setup` leaves it, then blocked by the
    // same update run terminalization applies.
    runtime
        .apply_task_automation_update(
            id,
            orbit_engine::TaskAutomationUpdate {
                job_run_id: Some(run_id.to_string()),
                ..orbit_engine::blocked_workflow_failure_update(
                    "task_pr_pipeline",
                    run_id,
                    Some("STEP_FAILED"),
                    Some(error),
                )
            },
        )
        .expect("block through the workflow-failure update");
}

fn missing_launcher_error(program: &std::path::Path) -> String {
    format!(
        "execution failed: v2 job dispatch: cli invocation failed (permanent): provider \
         launcher `{}` for provider `codex` was not found; searched: /usr/bin/codex",
        program.display()
    )
}

/// The re-check returns exactly the blocks whose launcher now resolves, with an
/// audit note, and leaves a still-missing launcher and a task-level failure
/// blocked. A second pass finds nothing left to do.
#[cfg(unix)]
#[test]
fn requeue_returns_only_cleared_infra_blocks_to_backlog_with_an_audit_note() {
    use std::os::unix::fs::PermissionsExt;

    let (root, runtime) = test_runtime();
    let installed = root.path().join("bin/codex");
    std::fs::create_dir_all(installed.parent().expect("bin dir")).expect("create bin dir");
    std::fs::write(&installed, "#!/bin/sh\nexit 0\n").expect("install launcher");
    std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755))
        .expect("make launcher executable");
    let still_missing = root.path().join("absent/codex");

    let cleared = add_task(&runtime, "Launcher installed since");
    let missing = add_task(&runtime, "Launcher still missing");
    let task_failure = add_task(&runtime, "Task-level failure");
    block_by_run_failure(
        &runtime,
        &cleared.id,
        "jrun-cleared",
        &missing_launcher_error(&installed),
    );
    block_by_run_failure(
        &runtime,
        &missing.id,
        "jrun-missing",
        &missing_launcher_error(&still_missing),
    );
    block_by_run_failure(
        &runtime,
        &task_failure.id,
        "jrun-task",
        "step `implement_one` completed with success=false",
    );

    let requeued = runtime
        .requeue_cleared_infra_blocked_tasks()
        .expect("requeue cleared blocks");

    assert_eq!(
        requeued
            .iter()
            .map(|task| task.task_id.as_str())
            .collect::<Vec<_>>(),
        vec![cleared.id.as_str()]
    );
    let status = |id: &str| runtime.get_task(id).expect("task").status;
    assert_eq!(status(&cleared.id), TaskStatus::Backlog);
    assert_eq!(status(&missing.id), TaskStatus::Blocked);
    assert_eq!(status(&task_failure.id), TaskStatus::Blocked);

    let history = runtime.get_task_history(&cleared.id).expect("history");
    let audit = history.last().expect("audit entry");
    assert_eq!(audit.event, "infra_block_cleared");
    assert_eq!(audit.from_status, Some(TaskStatus::Blocked));
    assert_eq!(audit.to_status, Some(TaskStatus::Backlog));
    let note = audit.note.as_deref().unwrap_or_default();
    assert!(
        note.contains(&installed.display().to_string()),
        "names where it resolves: {note}"
    );
    assert!(
        note.contains("run_id=jrun-cleared"),
        "names the block: {note}"
    );

    assert!(
        runtime
            .requeue_cleared_infra_blocked_tasks()
            .expect("second pass")
            .is_empty(),
        "a requeued task is no longer blocked, so a re-run is a no-op"
    );
}
