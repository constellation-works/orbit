//! The blocked-task triage's requeue fails closed on an unreadable count.

use orbit_types::task::TaskStatus;
use serde_json::json;

use crate::application::task::tests::{enter_isolated_child, test_runtime};
use crate::application::task::{
    BLOCKED_TASK_TRIAGED_EVENT, BlockTriageDecision, BlockTriageRequest, FinalRecoveryCompletion,
    FinalRecoveryRequest, FinalRecoveryRequeueBound, TaskAddParams, TaskUpdateParams,
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
            title: "Triage an escalated block".to_string(),
            description: "Exercise the triage requeue bound.".to_string(),
            acceptance_criteria: vec!["The block is triaged once.".to_string()],
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
    runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: task.id.clone(),
                run_id: "jrun-20261021-0100-t1".to_string(),
                observed: runtime.final_recovery_revision(&task).expect("observe"),
                repo_root: root.path().join("repo"),
                base_ref: "main".to_string(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: FinalRecoveryRequeueBound::default(),
            },
            Some(&json!({"decision": "escalate", "diagnosis": "worker crashed", "human_action": "requeue"})),
        )
        .expect("escalate");
    let before = runtime.get_task(&task.id).expect("read task");
    assert_eq!(before.status, TaskStatus::Blocked);
    let comments_before = runtime.get_task_comments(&task.id).expect("comments").len();

    super::super::apply::FAIL_REQUEUE_COUNT_READ.with(|fail| fail.set(true));
    let error = runtime
        .record_block_triage(&BlockTriageRequest {
            task_id: task.id.clone(),
            blocker: "worker crashed".to_string(),
            decision: BlockTriageDecision::Requeue {
                repair: "the host is back".to_string(),
            },
            requested_by: "claude".to_string(),
        })
        .expect_err("an unreadable count must refuse the requeue")
        .to_string();
    assert!(error.contains("cannot be read"), "{error}");
    let after = runtime.get_task(&task.id).expect("read task");
    assert_eq!(after.status, TaskStatus::Blocked, "nothing was requeued");
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
            .any(|entry| entry.event == BLOCKED_TASK_TRIAGED_EVENT),
        "the episode stays untriaged, so the next triage reads it again"
    );
}
