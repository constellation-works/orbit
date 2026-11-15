//! A deferred infra requeue records the redacted authentication error.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskReservationReleaseReason;
use orbit_types::task::{RecordedFinalRecovery, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    FailureActivityCheckpoint, JobRunState, TRANSIENT_INFRA_BLOCKED_EVENT,
    TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT, TransientInfraBlock, TransientInfrastructure,
};
use serde_json::json;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::params::TaskRecordUpdateParams;
use crate::application::task::records::TaskRecords;
use crate::application::task::transient_infra::PRESERVATION_FAULT;

const SECRET_PASSWORD: &str = "supersecretpassword";
const SECRET_TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn transient_infra_deferral_redacts_the_authentication_error() {
    if !enter_isolated_child(
        module_path!(),
        "transient_infra_deferral_redacts_the_authentication_error",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Infra blocked".into(),
            description: "The publication could not be authenticated.".into(),
            acceptance_criteria: vec!["The history note is redacted.".into()],
            ..Default::default()
        })
        .expect("add task");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .expect("insert run");
    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Interrupted,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("stop the run");
    let mut state = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({}),
    );
    state.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
        activity_name: "pr_failure_handoff".into(),
        failed_step_id: "git_push".into(),
        output: json!({
            "decision": "blocked_failure_pr",
            "pr_number": "17",
            "branch": "task/infra",
            "head_sha": "a".repeat(40),
            "task_spec_digest": "b".repeat(64),
        }),
    });
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("handoff receipt");
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .expect("block the task");
    let blocked = TransientInfraBlock {
        schema_version: 1,
        run_id: run.run_id.clone(),
        failed_step_id: "git_push".into(),
        class: TransientInfrastructure::StoreLocked,
    };
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "system".into(),
                append_history: vec![
                    TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: TRANSIENT_INFRA_BLOCKED_EVENT.into(),
                        note: Some(serde_json::to_string(&blocked).expect("block note")),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    },
                    TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: "final_recovery_decided".into(),
                        note: Some(format!("outcome=escalated (run_id={})", run.run_id)),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: Some(TaskDecision::FinalRecovery {
                            run_id: run.run_id.clone(),
                            decision: "escalate".into(),
                            outcome: RecordedFinalRecovery::Escalated { reason: None },
                            fault: Default::default(),
                            fault_evidence: String::new(),
                            fault_judged_by: None,
                        }),
                    },
                ],
                expected_status: Some(vec![TaskStatus::Blocked]),
                ..Default::default()
            },
        )
        .expect("record the block");

    let scanned = runtime
        .transient_infra_blocked_tasks()
        .expect("classify")
        .into_iter()
        .find(|block| block.task_id == task.id)
        .expect("the task is transient-infra blocked");
    assert!(scanned.cleared, "{scanned:?}");
    assert!(scanned.held.is_none(), "{scanned:?}");
    assert!(scanned.pull_request.is_some(), "{scanned:?}");

    let secret = format!(
        "authentication failed for https://user:{SECRET_PASSWORD}@github.com/org/repo.git ({SECRET_TOKEN})"
    );
    PRESERVATION_FAULT.with(|fault| *fault.borrow_mut() = Some(secret));
    runtime
        .requeue_cleared_transient_infra_blocks()
        .expect("deferral is recorded, not fatal");

    let history = runtime.get_task_history(&task.id).expect("history");
    let note = history
        .iter()
        .rev()
        .find(|entry| entry.event == TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT)
        .and_then(|entry| entry.note.clone())
        .expect("a deferral note");
    assert!(!note.contains(SECRET_PASSWORD), "{note}");
    assert!(!note.contains(SECRET_TOKEN), "{note}");
    assert!(note.contains("[REDACTED"), "{note}");

    let held = runtime
        .transient_infra_blocked_tasks()
        .expect("rescan")
        .into_iter()
        .find(|block| block.task_id == task.id)
        .and_then(|block| block.held)
        .expect("the deferral holds the block");
    assert!(!held.contains(SECRET_PASSWORD), "{held}");
    assert!(!held.contains(SECRET_TOKEN), "{held}");
    assert!(held.contains("[REDACTED"), "{held}");
}
