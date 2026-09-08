//! Human task transitions retain the status they read when they write.

use chrono::Utc;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};

use super::super::transitions::{TRANSITION_READ_HOOK_TEST_LOCK, set_transition_read_hook_status};
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
            workspace_path: Some(".".to_string()),
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

fn with_changed_status(id: &str, test: impl FnOnce()) {
    let _serial = TRANSITION_READ_HOOK_TEST_LOCK
        .lock()
        .expect("serialize transition read hooks");
    set_transition_read_hook_status(Some(id), Some(TaskStatus::Rejected));
    test();
    set_transition_read_hook_status(None, None);
}

#[test]
fn approve_does_not_overwrite_a_status_changed_after_its_read() {
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Approve CAS");
    set_status(&runtime, &task.id, TaskStatus::Review);

    with_changed_status(&task.id, || {
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

    with_changed_status(&task.id, || {
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

    with_changed_status(&task.id, || {
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

    with_changed_status(&task.id, || {
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
