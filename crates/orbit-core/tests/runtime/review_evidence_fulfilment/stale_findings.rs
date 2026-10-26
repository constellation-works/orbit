//! A same-status edit at the findings transition supersedes old analysis.

use std::cell::Cell;
use std::rc::Rc;

use orbit_core::application::task::TaskUpdateParams;
use orbit_core::test_support::with_review_evidence_completion;
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    REVIEW_EVIDENCE_FINDINGS_ARTIFACT, REVIEW_EVIDENCE_HOLD_ARTIFACT, ReviewEvidenceFinding,
};

use super::{CODEQL, Stub, artifact, held, latest_decision, seed_runtime_hold};
use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::RuntimeHost;
use serde_json::json;

#[test]
fn a_findings_transition_rechecks_the_hold_under_the_task_lock() {
    if !super::super::dispatch_admission::isolated(
        "review_evidence_fulfilment::stale_findings::a_findings_transition_rechecks_the_hold_under_the_task_lock",
    ) {
        return;
    }
    for change in ["meaning", "hold", "decision", "none"] {
        let (fixture, hold) = held(Stub::Findings, CODEQL);
        let finding = ReviewEvidenceFinding {
            check: hold.requirements[0].name.clone(),
            rule: "rust/cleartext-logging".into(),
            message: "credential logged".into(),
            file: Some("src/lease.rs".into()),
            line: Some(42),
            code_flow: vec![],
        };
        let runtime = fixture.runtime.clone();
        let task_id = fixture.task_id.clone();
        let mut replacement = hold.clone();
        replacement.attempt_id = "newer-review-attempt".into();
        let changed = Rc::new(Cell::new(false));
        let checkpoint = Rc::clone(&changed);
        let output = with_review_evidence_completion(
            vec![finding.clone()],
            move || {
                // Findings are already durable when the competing update wins.
                assert!(
                    runtime
                        .get_task_artifact(&task_id, REVIEW_EVIDENCE_FINDINGS_ARTIFACT)
                        .unwrap()
                        .is_some()
                );
                match change {
                    "meaning" => {
                        runtime
                            .update_task_as_human(
                                &task_id,
                                TaskUpdateParams {
                                    description: Some(
                                        "An operator changed the task meaning".into(),
                                    ),
                                    ..Default::default()
                                },
                                "Operator".into(),
                            )
                            .unwrap();
                    }
                    "hold" => runtime
                        .stores()
                        .tasks()
                        .with_task_write_lock(&task_id, &mut || {
                            seed_runtime_hold(&runtime, &task_id, &replacement);
                            Ok(())
                        })
                        .unwrap(),
                    "decision" => {
                        runtime
                            .stores()
                            .task_history()
                            .update_task_history(
                                &task_id,
                                orbit_store::contracts::TaskHistoryUpdateParams {
                                    actor: "system".into(),
                                    append_history: vec![TaskHistoryEntry {
                                        at: Utc::now(),
                                        by: "system".into(),
                                        event: "review_replaced".into(),
                                        note: None,
                                        from_status: Some(TaskStatus::InProgress),
                                        to_status: Some(TaskStatus::InProgress),
                                        operation_id: None,
                                        decision: None,
                                    }],
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                    }
                    "none" => {}
                    _ => unreachable!(),
                }
                assert_eq!(
                    runtime.get_task(&task_id).unwrap().status,
                    TaskStatus::InProgress
                );
                checkpoint.set(true);
            },
            || {
                fixture
                    .runtime
                    .run_deterministic(
                        "fulfil_review_evidence",
                        &json!({}),
                        &json!({
                            "task_id": fixture.task_id,
                            "hold_key": format!("{}:{}", hold.attempt_id, hold.candidate.commit),
                        }),
                        ToolContext {
                            reservation_owner: Some(ReservationOwnerContext {
                                owner_run_id: hold.run_id.clone(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .unwrap()
            },
        );
        assert!(changed.get(), "the transition checkpoint ran: {change}");
        let requeued = change == "none";
        assert_eq!(output["reason"], "findings_reported", "{change}: {output}");
        assert_eq!(
            output["requeued"], requeued,
            "stale findings must not queue a newer task: {change}: {output}"
        );
        let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
        assert_eq!(
            task.status,
            if requeued {
                TaskStatus::Backlog
            } else {
                TaskStatus::InProgress
            },
            "{change}"
        );
        if change == "meaning" {
            assert_eq!(task.description, "An operator changed the task meaning");
        }
        let persisted = artifact(&fixture, REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap();
        assert_eq!(
            persisted["attempt_id"],
            if change == "hold" {
                "newer-review-attempt"
            } else {
                &hold.attempt_id
            },
            "{change}: the persisted hold is preserved"
        );
        assert_eq!(
            latest_decision(&fixture),
            match change {
                "none" => "review_evidence_findings",
                "decision" => "review_replaced",
                _ => "review_awaiting_evidence",
            },
            "{change}"
        );
        let findings = artifact(&fixture, REVIEW_EVIDENCE_FINDINGS_ARTIFACT).unwrap();
        assert_eq!(findings["attempt_id"], hold.attempt_id);
        assert_eq!(
            findings["results"],
            json!([finding]),
            "audit findings survive: {change}"
        );
        assert!(
            !fixture
                .runtime
                .get_task_history(&fixture.task_id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "review_evidence_findings")
                || requeued,
            "{change}"
        );
    }
}
