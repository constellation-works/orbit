//! The shared requeue count fails closed: an unreadable count refuses a final-recovery requeue.

use orbit_types::task::TaskStatus;
use serde_json::json;

use crate::application::task::tests::{enter_isolated_child, test_runtime};
use crate::application::task::{
    FINAL_RECOVERY_REQUEUED_EVENT, FinalRecoveryCompletion, FinalRecoveryRequest,
    FinalRecoveryRequeueBound, TaskAddParams, TaskUpdateParams,
};

/// Fault injection is necessary: no public input makes the shared requeue
/// count unreadable while the rest of the task stays readable.
#[test]
fn an_unreadable_requeue_count_refuses_the_requeue_and_records_nothing() {
    if !enter_isolated_child(
        module_path!(),
        "an_unreadable_requeue_count_refuses_the_requeue_and_records_nothing",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Requeue under an unreadable count".to_string(),
            description: "Exercise the requeue bound.".to_string(),
            acceptance_criteria: vec!["The requeue fails closed.".to_string()],
            plan: "1) implement".to_string(),
            ..Default::default()
        })
        .expect("add task");
    let task = runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start task");
    let request = FinalRecoveryRequest {
        task_id: task.id.clone(),
        run_id: "jrun-20261021-0100-t1".to_string(),
        observed: runtime.final_recovery_revision(&task).expect("observe"),
        repo_root: root.path().join("repo"),
        base_ref: "main".to_string(),
        completion: FinalRecoveryCompletion::Review,
        requeue_bound: FinalRecoveryRequeueBound::default(),
    };
    let comments_before = runtime.get_task_comments(&task.id).expect("comments").len();

    super::super::apply::FAIL_REQUEUE_COUNT_READ.with(|fail| fail.set(true));
    let error = runtime
        .apply_final_recovery(
            &request,
            Some(&json!({"decision": "requeue", "reason": "the host is back"})),
        )
        .expect_err("an unreadable count must refuse the requeue")
        .to_string();
    assert!(
        error.contains("injected requeue count read failure"),
        "{error}"
    );
    let after = runtime.get_task(&task.id).expect("read task");
    assert_eq!(after.status, TaskStatus::InProgress, "nothing was requeued");
    assert_eq!(
        runtime.get_task_comments(&task.id).expect("comments").len(),
        comments_before,
        "nothing was recorded"
    );
    assert!(
        !runtime
            .get_task_history(&task.id)
            .expect("history")
            .iter()
            .any(|entry| entry.event == FINAL_RECOVERY_REQUEUED_EVENT),
        "no requeue was recorded"
    );
}
