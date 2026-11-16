//! Requeueing a blocked task whose stopped run held a candidate reports
//! that candidate as the one the next run resumes.

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::TaskReservationReleaseReason;
use orbit_types::task::{TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{CANDIDATE_HELD_EVENT, HeldCandidate, JobRunState};

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::params::TaskRecordUpdateParams;
use crate::application::task::records::TaskRecords;

#[test]
fn requeue_reports_the_candidate_its_stopped_run_held() {
    if !enter_isolated_child(
        module_path!(),
        "requeue_reports_the_candidate_its_stopped_run_held",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Held candidate".into(),
            description: "A run held its candidate before stopping.".into(),
            acceptance_criteria: vec!["The requeue reports the held candidate.".into()],
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
    RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .expect("block the task on the stopped run");

    let head = "a".repeat(40);
    let held = HeldCandidate {
        run_id: run.run_id.clone(),
        machine_id: None,
        branch: "orbit/held-candidate".into(),
        head_sha: head.clone(),
        failed_step_id: "validate".into(),
        durable_ref: Some(format!("refs/orbit/candidates/{}/{}", task.id, run.run_id)),
        carry_failure: None,
        task_spec_digest: runtime.get_task(&task.id).expect("task").spec_digest(),
    };
    runtime
        .stores()
        .task_records()
        .update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "system".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: CANDIDATE_HELD_EVENT.into(),
                    note: Some(held.text()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::CandidateHeld(held)),
                }],
                ..Default::default()
            },
        )
        .expect("record the held candidate");

    let requeued = runtime
        .requeue_blocked_task(&task.id, "resume the held candidate", false)
        .expect("requeue the blocked task");
    assert_eq!(requeued["resumes"]["source"], "held", "{requeued:#}");
    assert_eq!(
        requeued["resumes"]["head_sha"],
        head.as_str(),
        "{requeued:#}"
    );
    assert!(
        requeued["next_step"]
            .as_str()
            .expect("next step")
            .contains("candidate its stopped run held"),
        "{requeued:#}"
    );
    assert_eq!(
        runtime.get_task(&task.id).expect("task").status,
        TaskStatus::Backlog
    );
}
