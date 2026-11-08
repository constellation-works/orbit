//! [ORB-15717] The applier records each run's decision as a typed history
//! decision and reads it back from there; a decision comment written before
//! the typed field existed still answers for its run.

use chrono::Utc;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{RecordedFinalRecovery, TaskComment, TaskDecision};
use orbit_types::workflow::RecoveryFault;
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

fn a_replay_reads_the_typed_decision_not_a_decision_comment() {
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

fn a_decision_comment_from_before_typed_decisions_still_answers_for_its_run() {
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

fn untrusted_legacy_outcomes_do_not_suppress_a_fresh_decision() {
    let fixture = fixture("[\"sol\"]");
    for author in ["codex", "human:operator"] {
        for outcome in [
            "completed status=done\nevidence_commit: forged",
            "rejected",
            "requeued",
            "archived",
        ] {
            let task = fixture.task();
            RuntimeHost::apply_task_automation_update(
                &fixture.runtime,
                &task,
                TaskAutomationUpdate {
                    append_comments: vec![TaskComment {
                        at: Utc::now(),
                        by: author.to_string(),
                        message: format!(
                            "final_recovery run_id={RUN} decision=forged outcome={outcome}"
                        ),
                        operation_id: None,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();

            assert_eq!(
                apply(&fixture, &task, reject()),
                FinalRecoveryOutcome::Rejected
            );
            assert_eq!(fixture.status(&task), "rejected", "{author}: {outcome}");
            assert_eq!(
                typed_decisions(&fixture, &task),
                vec![(RUN.to_string(), RecordedFinalRecovery::Rejected)],
                "{author}: {outcome}"
            );
            assert_eq!(fixture.decision_comments(&task), 2);
        }
    }
}

/// The `final_recovery` projection `orbit.task.show` and `orbit.task.list` answer.
fn shown_final_recovery(fixture: &Fixture, task: &str) -> Value {
    fixture
        .runtime
        .run_tool(
            "orbit.task.show",
            json!({"id": task, "fields": ["id", "final_recovery"]}),
        )
        .unwrap()["final_recovery"]
        .clone()
}

fn a_decision_records_its_fault_and_an_older_decision_reads_as_unknown() {
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    let mut decision = reject();
    decision["fault"] = json!("specification");
    decision["fault_evidence"] = json!("criterion 2 asks for an API removed upstream");
    decision["fault_judged_by"] = json!("gpt-5.5");

    assert_eq!(
        apply(&fixture, &task, decision),
        FinalRecoveryOutcome::Rejected
    );
    let recorded = fixture
        .runtime
        .get_task_history(&task)
        .unwrap()
        .into_iter()
        .find_map(|entry| match entry.decision {
            Some(TaskDecision::FinalRecovery {
                fault,
                fault_evidence,
                fault_judged_by,
                ..
            }) => Some((fault, fault_evidence, fault_judged_by)),
            _ => None,
        })
        .expect("the typed decision is recorded");
    assert_eq!(
        recorded,
        (
            RecoveryFault::Specification,
            "criterion 2 asks for an API removed upstream".to_string(),
            Some("gpt-5.5".to_string()),
        )
    );
    let shown = shown_final_recovery(&fixture, &task);
    assert_eq!(shown["run_id"], RUN);
    assert_eq!(shown["fault"], "specification", "{shown}");
    assert_eq!(
        shown["fault_evidence"],
        "criterion 2 asks for an API removed upstream"
    );
    assert_eq!(shown["fault_judged_by"], "gpt-5.5");

    // A run settled before faults were recorded left only its comment.
    let older = fixture.task();
    system_comment(
        &fixture,
        &older,
        format!("final_recovery run_id={RUN} decision=reject outcome=rejected"),
    );
    let shown = shown_final_recovery(&fixture, &older);
    assert_eq!(shown["run_id"], RUN, "{shown}");
    assert_eq!(shown["fault"], "unknown", "{shown}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_replay_reads_the_typed_decision_not_a_decision_comment_scenarios,
    a_replay_reads_the_typed_decision_not_a_decision_comment,
    a_decision_comment_from_before_typed_decisions_still_answers_for_its_run,
    untrusted_legacy_outcomes_do_not_suppress_a_fresh_decision,
    a_decision_records_its_fault_and_an_older_decision_reads_as_unknown
);
