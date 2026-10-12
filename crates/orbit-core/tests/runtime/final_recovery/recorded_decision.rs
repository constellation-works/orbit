//! [ORB-15717] The applier records each run's decision as a typed history
//! decision and reads it back from there; a decision comment written before
//! the typed field existed still answers for its run.

use chrono::Utc;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{RecordedFinalRecovery, TaskComment, TaskDecision};
use serde_json::{Value, json};

use super::{Fixture, fixture};

const RUN: &str = "jrun-recorded-decision";

fn reject() -> Value {
    json!({
        "decision": "reject",
        "reason": "the task asks for a retired API",
        "evidence": "The API was removed upstream.",
    })
}

fn requeue() -> Value {
    json!({"decision": "requeue", "reason": "the missing tool now resolves"})
}

/// Apply `decision` as `RUN`'s final recovery, through the task applier.
fn apply(fixture: &Fixture, task: &str, decision: Value) -> FinalRecoveryOutcome {
    let observed = fixture
        .runtime
        .final_recovery_revision(&fixture.runtime.get_task(task).unwrap())
        .unwrap();
    fixture
        .runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: task.to_string(),
                run_id: RUN.to_string(),
                observed,
                repo_root: fixture.repo.clone(),
                base_ref: "main".to_string(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: Default::default(),
            },
            Some(&decision),
        )
        .unwrap()
}

/// The typed final-recovery decisions recorded on `task`, oldest first.
fn typed_decisions(fixture: &Fixture, task: &str) -> Vec<(String, RecordedFinalRecovery)> {
    fixture
        .runtime
        .get_task_history(task)
        .unwrap()
        .into_iter()
        .filter_map(|entry| match entry.decision {
            Some(TaskDecision::FinalRecovery {
                run_id, outcome, ..
            }) => Some((run_id, outcome)),
            _ => None,
        })
        .collect()
}

/// Append a `system` comment, as an applier that predates the typed
/// decision wrote its record.
fn system_comment(fixture: &Fixture, task: &str, message: String) {
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        task,
        TaskAutomationUpdate {
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message,
                operation_id: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn a_replay_reads_the_typed_decision_not_a_decision_comment() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::recorded_decision::a_replay_reads_the_typed_decision_not_a_decision_comment",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();

    assert_eq!(
        apply(&fixture, &task, reject()),
        FinalRecoveryOutcome::Rejected
    );
    assert_eq!(
        typed_decisions(&fixture, &task),
        vec![(RUN.to_string(), RecordedFinalRecovery::Rejected)],
        "the applier records its decision as a typed history decision"
    );

    // A comment naming the same run with another outcome is not the record.
    system_comment(
        &fixture,
        &task,
        format!("final_recovery run_id={RUN} decision=requeue outcome=requeued"),
    );

    assert_eq!(
        apply(&fixture, &task, requeue()),
        FinalRecoveryOutcome::Rejected,
        "a replay answers from the typed decision"
    );
    assert_eq!(fixture.status(&task), "rejected");
    assert_eq!(typed_decisions(&fixture, &task).len(), 1);
}

#[test]
fn a_decision_comment_from_before_typed_decisions_still_answers_for_its_run() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::recorded_decision::a_decision_comment_from_before_typed_decisions_still_answers_for_its_run",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    // An older applier settled this run and recorded only its comment.
    system_comment(
        &fixture,
        &task,
        format!("final_recovery run_id={RUN} decision=reject outcome=rejected"),
    );

    assert_eq!(
        apply(&fixture, &task, requeue()),
        FinalRecoveryOutcome::Rejected,
        "the legacy comment is the run's recorded outcome"
    );
    assert_eq!(
        fixture.status(&task),
        "in-progress",
        "an already-applied decision is not applied again"
    );
    assert!(typed_decisions(&fixture, &task).is_empty());
    assert_eq!(fixture.decision_comments(&task), 1);
}
