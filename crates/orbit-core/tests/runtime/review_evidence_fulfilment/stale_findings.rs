//! Findings reject stale holds and exclude writers between checking and requeueing.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use orbit_common::fs::io::{FileLockOptions, with_exclusive_file_lock_options};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};

use orbit_core::application::task::TaskUpdateParams;
use orbit_core::test_support::with_review_evidence_completion;
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    REVIEW_EVIDENCE_FINDINGS_ARTIFACT, REVIEW_EVIDENCE_HOLD_ARTIFACT, ReviewEvidenceFinding,
    ReviewEvidenceHold,
};

use super::{CODEQL, Fixture, Stub, artifact, held, latest_decision, seed_runtime_hold};
use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::RuntimeHost;
use serde_json::{Value, json};

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
                apply_findings_change(&runtime, &task_id, &replacement, change).unwrap();
                assert_eq!(
                    runtime.get_task(&task_id).unwrap().status,
                    TaskStatus::InProgress
                );
                checkpoint.set(true);
            },
            || {},
            || fulfil_findings(&fixture, &hold),
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

/// Uses the same mutation paths as the stale-before-check cases. Raw hold bytes
/// model a system-owned replacement; operator APIs cannot replace that artifact.
fn apply_findings_change(
    runtime: &OrbitRuntime,
    task_id: &str,
    replacement: &ReviewEvidenceHold,
    change: &str,
) -> Result<(), OrbitError> {
    match change {
        "meaning" => {
            runtime.update_task_as_human(
                task_id,
                TaskUpdateParams {
                    description: Some("An operator changed the task meaning".into()),
                    ..Default::default()
                },
                "Operator".into(),
            )?;
        }
        "hold" => runtime
            .stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                seed_runtime_hold(runtime, task_id, replacement);
                Ok(())
            })?,
        "decision" => {
            let status = runtime.get_task(task_id)?.status;
            runtime.stores().task_history().update_task_history(
                task_id,
                orbit_store::contracts::TaskHistoryUpdateParams {
                    actor: "system".into(),
                    append_history: vec![TaskHistoryEntry {
                        at: Utc::now(),
                        by: "system".into(),
                        event: "review_replaced".into(),
                        note: None,
                        from_status: Some(status),
                        to_status: Some(status),
                        operation_id: None,
                        decision: None,
                    }],
                    ..Default::default()
                },
            )?;
        }
        "none" => {}
        _ => unreachable!(),
    }
    Ok(())
}

#[test]
fn a_findings_transition_excludes_a_competing_meaning_edit() {
    assert_findings_writer_exclusion(
        "meaning",
        "a_findings_transition_excludes_a_competing_meaning_edit",
    );
}

#[test]
fn a_findings_transition_excludes_a_competing_hold_replacement() {
    assert_findings_writer_exclusion(
        "hold",
        "a_findings_transition_excludes_a_competing_hold_replacement",
    );
}

#[test]
fn a_findings_transition_excludes_a_competing_delivery_decision() {
    assert_findings_writer_exclusion(
        "decision",
        "a_findings_transition_excludes_a_competing_delivery_decision",
    );
}

fn assert_findings_writer_exclusion(change: &'static str, test: &str) {
    if !super::super::dispatch_admission::isolated(&format!(
        "review_evidence_fulfilment::stale_findings::{test}"
    )) {
        return;
    }
    let (fixture, hold) = held(Stub::Findings, CODEQL);
    let runtime = fixture.runtime.clone();
    let task_id = fixture.task_id.clone();
    let mut replacement = hold.clone();
    replacement.attempt_id = "newer-review-attempt".into();
    let finding = ReviewEvidenceFinding {
        check: hold.requirements[0].name.clone(),
        rule: "rust/cleartext-logging".into(),
        message: "credential logged".into(),
        file: Some("src/lease.rs".into()),
        line: Some(42),
        code_flow: vec![],
    };
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    // This is the stable target that the task bundle's writers share. Using
    // their re-entrant lock helper lets the worker attempt the real update with
    // a zero-wait acquisition policy, instead of inferring exclusion from how
    // long a scheduled thread has been silent. No extra lock protects the test.
    let lock_target = registry
        .canonical_task_bundle_path(&runtime.workspace_id().unwrap(), &task_id)
        .unwrap()
        .with_extension("bundle");
    let deadline = orbit_common::test_env::FIXTURE_STEP_DEADLINE;
    let excluded = Rc::new(Cell::new(false));
    let checkpoint = Rc::clone(&excluded);
    let output = std::thread::scope(|scope| {
        let (attempt_tx, attempt_rx) = mpsc::sync_channel(1);
        let (observed_tx, observed_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let writer = scope.spawn(move || {
            // Both this readiness wait and the retry wait end on disconnect,
            // including when fulfilment unwinds. Scope joins the worker on
            // every path; file lock acquisition also has a bounded deadline.
            if attempt_rx.recv_timeout(deadline).is_err() {
                return;
            }
            let attempt = with_exclusive_file_lock_options(
                &lock_target,
                "competing findings update",
                FileLockOptions {
                    timeout: Duration::ZERO,
                    ..Default::default()
                },
                || apply_findings_change(&runtime, &task_id, &replacement, change),
            );
            let was_excluded = match attempt {
                Ok(()) => false,
                Err(error) if error.file_lock_timeout().is_some() => true,
                Err(error) => panic!("unexpected competing update error: {error}"),
            };
            observed_tx.send(was_excluded).unwrap();
            if resume_rx.recv_timeout(deadline).is_err() {
                return;
            }
            let status_before_retry = runtime.get_task(&task_id).unwrap().status;
            if was_excluded {
                apply_findings_change(&runtime, &task_id, &replacement, change).unwrap();
            }
            done_tx.send(status_before_retry).unwrap();
        });
        let output = with_review_evidence_completion(
            vec![finding.clone()],
            || {},
            move || {
                // Only reached after the current-hold check has succeeded,
                // immediately before the mutation that writes Backlog.
                attempt_tx.send(()).unwrap();
                checkpoint.set(observed_rx.recv_timeout(deadline).unwrap());
            },
            || fulfil_findings(&fixture, &hold),
        );
        resume_tx.send(()).unwrap();
        let status_before_retry = done_rx.recv_timeout(deadline).unwrap();
        writer.join().unwrap();
        assert_eq!(status_before_retry, TaskStatus::Backlog);
        output
    });
    // Check after releasing and joining the writer, even in the mutation
    // control where it successfully commits inside the check-to-write gap.
    assert!(
        excluded.get(),
        "{change}: competing writer committed between the current-hold check and backlog write"
    );
    assert_eq!(output["reason"], "findings_reported");
    assert_eq!(output["requeued"], true);
    let task = fixture.runtime.get_task(&fixture.task_id).unwrap();
    assert_eq!(task.status, TaskStatus::Backlog);
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
        }
    );
    let history = fixture.runtime.get_task_history(&fixture.task_id).unwrap();
    let transition = history
        .iter()
        .position(|entry| entry.event == "review_evidence_findings")
        .unwrap();
    if change == "decision" {
        let later = history
            .iter()
            .position(|entry| entry.event == "review_replaced")
            .unwrap();
        assert!(
            later > transition,
            "the later delivery decision survives the requeue"
        );
        assert_eq!(latest_decision(&fixture), "review_replaced");
    } else {
        assert_eq!(latest_decision(&fixture), "review_evidence_findings");
    }
    let findings = artifact(&fixture, REVIEW_EVIDENCE_FINDINGS_ARTIFACT).unwrap();
    assert_eq!(findings["attempt_id"], hold.attempt_id);
    assert_eq!(
        findings["results"],
        json!([finding]),
        "audit findings survive"
    );
}

fn fulfil_findings(fixture: &Fixture, hold: &ReviewEvidenceHold) -> Value {
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
}
