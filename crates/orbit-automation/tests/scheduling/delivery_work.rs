//! Regressions for count-only overfill and discarded partial examination.

use super::delivery::*;
use orbit_automation::delivery::{self as scheduler, evidence::EvidenceFacts};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::task::TaskComplexity::{self, *};
use orbit_types::workflow::automation::*;
use std::sync::{Arc, atomic::Ordering};

fn reviews(
    complexities: &[TaskComplexity],
    budget: Option<DeliveryReviewBudget>,
) -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    let mut trigger = trigger();
    trigger.threshold = 1;
    trigger.coverage = CoverageClass::LandedCodeReviewV1;
    trigger.max_items = 20;
    trigger.review_budget = budget;
    evaluate(store.as_ref(), &host, &trigger, true);
    for (index, complexity) in complexities.iter().enumerate() {
        host.complexities
            .lock()
            .unwrap()
            .insert(landing(index + 1).key, vec![*complexity; 2]);
    }
    host.page(0, complexities.len());
    (store, host, trigger)
}

#[test]
fn review_point_edges_keep_the_oldest_prefix_and_freeze_overrides() {
    for (pending, expected) in [
        (vec![Hard, Low], 1),
        (vec![Medium, Medium, Low], 2),
        (vec![Medium, Low, Low, Low, Low, Low], 5),
        (vec![Low; 9], 8),
        (vec![Low, Hard, Low], 1),
    ] {
        let (store, host, trigger) = reviews(&pending, None);
        let state = evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap();
        let batch = state.active.unwrap().batch;
        assert_eq!(batch.deliveries.len(), expected);
        assert_eq!(batch.through_inclusive, revision(expected));
        assert_eq!(state.pending.len(), pending.len());
        let work = batch.review_work.unwrap();
        assert!(work.total_points <= work.budget.max_points);
        assert_eq!(work.charges.len(), expected);
    }
    let budget = DeliveryReviewBudget {
        max_points: 6,
        low: 2,
        medium: 3,
        hard: 5,
        ..Default::default()
    };
    let (store, host, trigger) = reviews(&[Medium, Medium, Low], Some(budget.clone()));
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let frozen = state.active.unwrap();
    assert_eq!(frozen.batch.deliveries.len(), 2);
    assert_eq!(frozen.batch.review_work.as_ref().unwrap().budget, budget);
    host.complexities.lock().unwrap().clear();
    host.page(3, 3);
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .active
            .unwrap()
            .batch,
        frozen.batch
    );
}

#[test]
fn size_and_assessment_holds_never_skip_or_cover_debt() {
    for (workload, reason) in [
        (
            ReviewWorkload {
                files: 21,
                lines: 1,
                binary_files: 0,
            },
            "files",
        ),
        (
            ReviewWorkload {
                files: 1,
                lines: 2001,
                binary_files: 0,
            },
            "lines",
        ),
        (
            ReviewWorkload {
                files: 1,
                lines: 0,
                binary_files: 1,
            },
            "binary",
        ),
    ] {
        let (store, host, trigger) = reviews(&[Low, Low], None);
        host.workloads
            .lock()
            .unwrap()
            .insert("c0..c2".into(), workload.clone());
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        let state = result.state.unwrap();
        let batch = state.active.unwrap().batch;
        assert_eq!(batch.deliveries.len(), 1);
        assert!(batch.review_work.unwrap().stop_reason.contains(reason));
        assert_eq!(state.covered, revision(0));
        assert_eq!(state.pending.len(), 2);
        let (store, host, trigger) = reviews(&[Low], None);
        host.workloads
            .lock()
            .unwrap()
            .insert("c0..c1".into(), workload);
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        assert!(result.reason.contains("review_work_oversize"));
        let state = result.state.unwrap();
        assert!(state.active.is_none());
        assert_eq!(state.covered, revision(0));
        assert_eq!(state.pending.len(), 1);
    }
    for complexity in [Unassessed, XHard] {
        let (store, host, mut trigger) = reviews(&[complexity], None);
        trigger.threshold = 1;
        // Use age triggering without changing the persisted trigger's identity.
        let result = evaluate_at(store.as_ref(), &host, &trigger, true, 61).unwrap();
        assert!(result.state.unwrap().active.is_none());
        assert!(result.reason.contains(if complexity == Unassessed {
            "assessment_required"
        } else {
            "oversize"
        }));
    }
}

fn facts(attempt: &BatchAttempt, evidence: &CoverageEvidence) -> EvidenceFacts {
    let bytes = serde_json::to_vec(evidence).unwrap();
    EvidenceFacts {
        artifact_digest: scheduler::digest(&bytes),
        bytes,
        reference: "task:original/artifact".into(),
        submitted_by: "run:original".into(),
        authorized: true,
        source_verified: true,
        verified_progress: attempt
            .progress
            .iter()
            .map(|snapshot| snapshot.evidence_digest.clone())
            .collect(),
        submission: None,
        finalization_refusal: None,
        action_stopped: true,
        changed_paths: attempt
            .batch
            .deliveries
            .iter()
            .map(|d| {
                (
                    d.key.clone(),
                    vec![format!("src/{}.rs", d.after.commit), "Cargo.lock".into()],
                )
            })
            .collect(),
    }
}

#[test]
fn retry_keeps_only_explicit_completed_evidence_and_rejects_forged_carry() {
    let (store, host, trigger) = reviews(&[Medium, Medium], None);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.evidence(&first);
    let mut partial = host.evidence.lock().unwrap().clone().unwrap();
    partial.examination_complete = false;
    partial.delivery_examinations[1].completed = false;
    partial.delivery_examinations[1].examined_paths.clear();
    let original_bytes = serde_json::to_vec(&partial).unwrap();
    *host.evidence.lock().unwrap() = Some(partial.clone());
    host.stopped.store(true, Ordering::SeqCst);
    host.page(2, 2);
    let retried = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.progress.len(), 1);
    assert_eq!(retried.progress[0].evidence, original_bytes);
    assert_eq!(
        retried.progress[0].action_id,
        first.action_id.clone().unwrap()
    );
    assert_eq!(
        retried.progress[0].deliveries,
        vec![first.batch.deliveries[0].key.clone()]
    );
    *host.evidence.lock().unwrap() = None;
    host.stopped.store(false, Ordering::SeqCst);
    let second = evaluate_at(store.as_ref(), &host, &trigger, true, 6)
        .unwrap()
        .state
        .unwrap()
        .active
        .unwrap();
    let mut complete = evidence_template(&second);
    assert!(complete.delivery_examinations[0].source_digest.is_some());
    assert!(!complete.delivery_examinations[1].completed);
    assert_eq!(complete.findings, partial.findings);
    complete.delivery_examinations[1] = partial.delivery_examinations[1].clone();
    complete.delivery_examinations[1].completed = true;
    complete.delivery_examinations[1].examined_paths =
        vec!["src/c2.rs".into(), "Cargo.lock".into()];
    complete.examination_complete = true;
    let valid = facts(&second, &complete);
    assert!(scheduler::evidence::validate(&second, &valid, now()).is_ok());
    let mut unauthorized = facts(&second, &complete);
    unauthorized.verified_progress.clear();
    assert!(scheduler::evidence::validate(&second, &unauthorized, now()).is_err());
    unauthorized = facts(&second, &complete);
    unauthorized.authorized = false;
    assert!(scheduler::evidence::validate(&second, &unauthorized, now()).is_err());
    for mutate in [
        (|e: &mut CoverageEvidence| e.batch_id.push('x')) as fn(&mut CoverageEvidence),
        |e| e.consumer.push('x'),
        |e| e.epoch.push('x'),
        |e| e.input_digest.push('x'),
        |e| e.from_exclusive.commit.push('x'),
        |e| e.action_id.push('x'),
        |e| e.attempt += 1,
        |e| e.delivery_examinations[0].rationale.push('x'),
        |e| e.findings.clear(),
    ] {
        let mut forged = complete.clone();
        mutate(&mut forged);
        assert!(scheduler::evidence::validate(&second, &facts(&second, &forged), now()).is_err());
    }
    let mut tampered = second.clone();
    tampered.progress[0].evidence.push(b' ');
    assert!(scheduler::evidence::validate(&tampered, &facts(&tampered, &complete), now()).is_err());
    for mutate in [
        (|e: &mut CoverageEvidence| e.batch_id.push('x')) as fn(&mut CoverageEvidence),
        |e| e.consumer.push('x'),
        |e| e.epoch.push('x'),
        |e| e.input_digest.push('x'),
        |e| e.through_inclusive.tree.push('x'),
        |e| e.action_id.push('x'),
        |e| e.attempt += 1,
    ] {
        let mut forged = second.clone();
        let mut prior: CoverageEvidence =
            serde_json::from_slice(&forged.progress[0].evidence).unwrap();
        mutate(&mut prior);
        forged.progress[0].evidence = serde_json::to_vec(&prior).unwrap();
        forged.progress[0].evidence_digest = scheduler::digest(&forged.progress[0].evidence);
        let mut current = complete.clone();
        current.delivery_examinations[0].source_digest =
            Some(forged.progress[0].evidence_digest.clone());
        assert!(
            scheduler::evidence::validate(&forged, &facts(&forged, &current), now()).is_err(),
            "even a rehashed prior artifact must match the frozen origin"
        );
    }
    *host.evidence.lock().unwrap() = Some(complete);
    let settled = evaluate_at(store.as_ref(), &host, &trigger, true, 7).unwrap();
    assert_eq!(settled.state.unwrap().covered, revision(2));
    assert_eq!(settled.receipts.len(), 1);
}

#[test]
fn provisional_and_skipped_entries_cannot_become_retry_progress() {
    for skipped in [false, true] {
        let (store, host, trigger) = reviews(&[Medium, Medium], None);
        let first = evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .active
            .unwrap();
        host.evidence(&first);
        let mut partial = host.evidence.lock().unwrap().clone().unwrap();
        partial.examination_complete = false;
        for entry in &mut partial.delivery_examinations {
            entry.completed = false;
        }
        if skipped {
            partial.delivery_examinations[0].completed = true;
            partial.delivery_examinations[0].examined_paths.pop();
            partial.delivery_examinations[0].skipped_paths = vec![SkippedPath {
                path: "Cargo.lock".into(),
                reason: "not read".into(),
            }];
        }
        *host.evidence.lock().unwrap() = Some(partial);
        host.stopped.store(true, Ordering::SeqCst);
        host.page(2, 2);
        let state = evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap();
        assert!(state.active.unwrap().progress.is_empty());
        assert_eq!(state.covered, revision(0));
    }
}

#[test]
fn concurrent_review_admission_and_restarted_host_keep_the_same_frozen_accounting() {
    let (store, host, trigger) = reviews(&[Medium, Medium, Low], None);
    let host = Arc::new(host);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| {
                let host = &host;
                let store = &store;
                let trigger = &trigger;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    evaluate_at(store.as_ref(), host, trigger, true, 0)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _outcome = handle.join().unwrap();
        }
    });
    let frozen = store
        .automation_state(CONSUMER)
        .unwrap()
        .unwrap()
        .active
        .unwrap();
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    assert_eq!(frozen.batch.deliveries.len(), 2);
    let restarted = Host::new();
    restarted.page(3, 3);
    let after = evaluate(store.as_ref(), &restarted, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(after.batch, frozen.batch);
    assert_eq!(after.input_digest, frozen.input_digest);
    assert!(restarted.actions.lock().unwrap().is_empty());
}
