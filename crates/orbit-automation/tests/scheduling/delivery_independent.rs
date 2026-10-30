//! Incident regressions through the public evaluator, recovery API and SQLite.

use super::delivery::{CONSUMER, Host, evaluate, evaluate_at, landing, now, revision, trigger};
use orbit_automation::delivery::{self, recovery};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::{recovery::RecoveryRequest, *};
use std::sync::{Arc, atomic::Ordering};

fn review_fixture() -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    host.independent.store(true, Ordering::SeqCst);
    let trigger = DeliveryTrigger {
        coverage: CoverageClass::LandedCodeReviewV1,
        ..trigger()
    };
    evaluate(store.as_ref(), &host, &trigger, true);
    (store, host, trigger)
}

fn recovery_request<'a>(
    trigger: &'a DeliveryTrigger,
    request: &'a RecoveryRequest,
) -> recovery::Recovery<'a> {
    recovery::Recovery {
        consumer: CONSUMER,
        epoch: "v1",
        trigger,
        repository: "owner/repo",
        host_refusal: None,
        request,
        by: "operator",
        now: now(),
        replay: None,
        resolved_action_id: None,
        expected_generation: None,
        action_terminal: false,
        action_failed_without_evidence: false,
    }
}

#[test]
fn open_review_does_not_hold_later_deliveries() {
    if super::isolated("delivery_independent::open_review_does_not_hold_later_deliveries") {
        return;
    }
    let (store, host, trigger) = review_fixture();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true);
    let a = first.state.unwrap().active.unwrap();
    host.page(2, 4);
    let later = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(later.observed, revision(4));
    assert_eq!(later.covered, revision(0));
    assert_eq!(later.active.as_ref().unwrap().batch, a.batch);
    assert_eq!(
        host.actions.lock().unwrap().len(),
        2,
        "an open review must not serialize distinct landed review ranges"
    );
    let b = &later.reservations[0];
    assert_eq!(b.batch.from_exclusive, a.batch.through_inclusive);
    assert_eq!(b.batch.commits, ["c3", "c4"]);
    assert!(
        a.batch
            .commits
            .iter()
            .all(|sha| !b.batch.commits.contains(sha))
    );

    host.page(4, 4);
    host.evidence(b);
    let gap = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(gap.covered, revision(0), "B's receipt cannot cover A's gap");
    assert_eq!(gap.reservations[0].state, BatchState::Covered);
    let b_receipt = store
        .automation_receipt(CONSUMER, &b.batch.id)
        .unwrap()
        .unwrap();
    assert_eq!(b_receipt.action_id, b.action_id.as_deref().unwrap());
    // Duplicate evidence cannot remint B or overwrite its accepted bytes.
    evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(
        store
            .automation_receipt(CONSUMER, &b.batch.id)
            .unwrap()
            .unwrap(),
        b_receipt
    );
    host.evidence(&a);
    let closed = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(closed.covered, revision(4));
    assert!(closed.active.is_none() && closed.reservations.is_empty());
    assert!(closed.pending.is_empty() && closed.pending_commits.is_empty());
    assert_eq!(store.automation_receipts(CONSUMER, 100).unwrap().len(), 2);
    assert_eq!(
        host.actions.lock().unwrap().len(),
        2,
        "B must never be reviewed again"
    );
}

#[test]
fn exhausted_review_reissues_only_its_batch_and_rejects_old_or_incomplete_answers() {
    if super::isolated(
        "delivery_independent::exhausted_review_reissues_only_its_batch_and_rejects_old_or_incomplete_answers",
    ) {
        return;
    }
    let (store, host, mut trigger) = review_fixture();
    trigger.retries = 0;
    host.page(0, 2);
    let a = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.failed.store(true, Ordering::SeqCst);
    host.page(2, 4);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Exhausted);
    assert_eq!(
        state.reservations.len(),
        1,
        "exhaustion must not hold B or observation"
    );
    let b = state.reservations[0].clone();
    host.failed.store(false, Ordering::SeqCst);
    host.page(4, 4);
    host.evidence(&b);
    let before = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let receipt = store
        .automation_receipt(CONSUMER, &b.batch.id)
        .unwrap()
        .unwrap();
    let ambiguous = RecoveryRequest {
        reissue_action: true,
        reason: "finish the unpaid range".into(),
        ..Default::default()
    };
    assert!(recovery::apply(store.as_ref(), &recovery_request(&trigger, &ambiguous)).is_err());
    assert_eq!(store.automation_state(CONSUMER).unwrap().unwrap(), before);
    let request = RecoveryRequest {
        batch_id: Some(a.batch.id.clone()),
        ..ambiguous
    };
    let preview = recovery::apply(store.as_ref(), &recovery_request(&trigger, &request)).unwrap();
    assert_eq!(preview.actions.len(), 2);
    assert_eq!(preview.actions[1].state, BatchState::Covered);
    let recovered = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(recovered.reservations, before.reservations);
    assert_eq!(recovered.active.as_ref().unwrap().batch, a.batch);
    let reissued = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(reissued.attempt, 2);
    assert_ne!(reissued.action_id, a.action_id);
    // A late clean answer from the original task cannot certify the reissue.
    host.evidence(&a);
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .covered,
        revision(0)
    );
    host.evidence(&reissued);
    host.evidence
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .examination_complete = false;
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .covered,
        revision(0)
    );
    assert!(
        store
            .automation_receipt(CONSUMER, &a.batch.id)
            .unwrap()
            .is_none()
    );
    host.evidence(&reissued);
    let closed = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(closed.covered, revision(4));
    assert_eq!(
        store
            .automation_receipt(CONSUMER, &b.batch.id)
            .unwrap()
            .unwrap(),
        receipt
    );
    assert_eq!(host.actions.lock().unwrap().len(), 3);
}

#[test]
fn later_review_can_be_recovered_while_the_oldest_is_running() {
    if super::isolated(
        "delivery_independent::later_review_can_be_recovered_while_the_oldest_is_running",
    ) {
        return;
    }
    let (store, host, trigger) = review_fixture();
    host.page(0, 4);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    host.page(4, 4);
    let mut before = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    // Public store settlement deterministically injects the terminal outcome
    // for B alone; A's executor remains live.
    let b = before.reservations[0].clone();
    let mut failed = before.clone();
    failed.generation += 1;
    failed.reservations[0].state = BatchState::Failed;
    failed.reservations[0].reason = Some("executor stopped".into());
    assert!(store.automation_commit(&before, &failed, None).unwrap());
    before = failed;
    let request = RecoveryRequest {
        batch_id: Some(b.batch.id.clone()),
        reissue_action: true,
        reason: "recover the later failed review".into(),
        ..Default::default()
    };
    let projection =
        recovery::apply(store.as_ref(), &recovery_request(&trigger, &request)).unwrap();
    assert_eq!(projection.action.unwrap().batch_id, b.batch.id);
    let next = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(next.active, before.active);
    assert_eq!(
        next.active.as_ref().unwrap().batch,
        first.active.unwrap().batch
    );
    assert_eq!(next.reservations[0].attempt, b.attempt + 1);
    assert_eq!(next.reservations[0].batch, b.batch);
}

#[test]
fn legacy_twelve_delivery_attempt_three_survives_upgrade_and_old_writers_fail_closed() {
    if super::isolated(
        "delivery_independent::legacy_twelve_delivery_attempt_three_survives_upgrade_and_old_writers_fail_closed",
    ) {
        return;
    }
    let layout = std::env::current_dir().unwrap().join("layout");
    std::fs::create_dir_all(layout.join("state")).unwrap();
    std::fs::write(layout.join("state/layout.version"), "3\n").unwrap();
    let upgrade = orbit_store::workflow::layout::upgrade_workspace_layout(&layout).unwrap();
    assert_eq!(upgrade.applied.len(), 1);
    assert_eq!(upgrade.applied[0].name, "independent-review-checkpoints");
    let compatibility =
        serde_json::from_slice(&std::fs::read(layout.join("state/layout.compat")).unwrap())
            .unwrap();
    let old_open = orbit_store::contracts::evaluate_newer_state(
        orbit_store::contracts::StateComponent::WorkspaceLayout,
        4,
        3,
        Some(compatibility),
        false,
    );
    assert!(
        old_open.is_err(),
        "a layout-v3 writer must be refused before any new state mutation"
    );
    assert!(
        orbit_store::workflow::layout::upgrade_workspace_layout(&layout)
            .unwrap()
            .applied
            .is_empty()
    );
    let raw_store = Store::open_in_memory().unwrap();
    let connection = raw_store.connection();
    let store = compose::automation_store(raw_store).unwrap();
    let host = Host::new();
    let earlier_trigger = DeliveryTrigger {
        coverage: CoverageClass::LandedCodeReviewV1,
        threshold: 1,
        max_items: 1,
        retries: 0,
        ..trigger()
    };
    evaluate(store.as_ref(), &host, &earlier_trigger, true);
    host.page(0, 1);
    let prior = evaluate(store.as_ref(), &host, &earlier_trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.evidence(&prior);
    host.page(1, 1);
    evaluate(store.as_ref(), &host, &earlier_trigger, true);
    let prior_receipt = store
        .automation_receipt(CONSUMER, &prior.batch.id)
        .unwrap()
        .unwrap();
    *host.evidence.lock().unwrap() = None;
    host.page(1, 2);
    let waived = evaluate(store.as_ref(), &host, &earlier_trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.failed.store(true, Ordering::SeqCst);
    host.page(2, 2);
    evaluate(store.as_ref(), &host, &earlier_trigger, true);
    delivery::waive(
        store.as_ref(),
        CONSUMER,
        &WaiveBatchRequest {
            batch_id: waived.batch.id,
            reason: "historical explicit waiver retained by upgrade".into(),
        },
        "operator",
        now(),
    )
    .unwrap();
    host.failed.store(false, Ordering::SeqCst);
    host.page(2, 3);
    host.page.lock().unwrap().exclusions.insert(
        landing(3).key,
        DeliveryExclusion {
            attempt_id: "before-pr-certificate".into(),
            assurance: "exact_tree".into(),
            task_meaning_digest: "meaning".into(),
            final_candidate_tree: revision(3).tree,
        },
    );
    evaluate(store.as_ref(), &host, &earlier_trigger, true);
    host.page(3, 4);
    {
        let mut page = host.page.lock().unwrap();
        page.deliveries.clear();
        page.unresolved
            .insert("c4".into(), "provider identity unavailable".into());
    }
    evaluate(store.as_ref(), &host, &earlier_trigger, true);
    let legacy_trigger = DeliveryTrigger {
        threshold: 12,
        max_items: 12,
        retries: 1,
        ..earlier_trigger
    };
    recovery::apply(
        store.as_ref(),
        &recovery_request(
            &legacy_trigger,
            &RecoveryRequest {
                adopt_settings: true,
                reason: "freeze the historical twelve-delivery range".into(),
                ..Default::default()
            },
        ),
    )
    .unwrap();
    host.page(4, 16);
    let frozen = evaluate(store.as_ref(), &host, &legacy_trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(frozen.batch.deliveries.len(), 12);
    host.page(16, 16);
    host.failed.store(true, Ordering::SeqCst);
    evaluate(store.as_ref(), &host, &legacy_trigger, true);
    evaluate_at(store.as_ref(), &host, &legacy_trigger, true, 6).unwrap();
    evaluate_at(store.as_ref(), &host, &legacy_trigger, true, 7).unwrap();
    host.failed.store(false, Ordering::SeqCst);
    let current = DeliveryTrigger {
        threshold: 5,
        max_items: 5,
        ..legacy_trigger.clone()
    };
    let request = RecoveryRequest {
        adopt_settings: true,
        reissue_action: true,
        reason: "retain the twelve-delivery recovery; cap only future reviews".into(),
        ..Default::default()
    };
    recovery::apply(store.as_ref(), &recovery_request(&current, &request)).unwrap();
    let legacy = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(legacy.active.as_ref().unwrap().attempt, 3);
    assert_eq!(legacy.active.as_ref().unwrap().batch, frozen.batch);
    let old_json: String = connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT state_json FROM automation_consumers WHERE consumer=?1",
            [CONSUMER],
            |row| row.get(0),
        )
        .unwrap();
    #[derive(serde::Deserialize)]
    struct OldRequiredEpoch {
        epoch: String,
    }
    assert_eq!(
        serde_json::from_str::<OldRequiredEpoch>(&old_json)
            .unwrap()
            .epoch,
        "v1"
    );
    host.independent.store(true, Ordering::SeqCst);
    host.page(16, 21);
    let next = evaluate_at(store.as_ref(), &host, &current, true, 8)
        .unwrap()
        .state
        .unwrap();
    assert_eq!(next.active.as_ref().unwrap().attempt, 3);
    assert_eq!(next.active.as_ref().unwrap().batch, frozen.batch);
    assert_eq!(legacy.waived.len(), 1);
    assert_eq!(legacy.excluded.len(), 1);
    assert_eq!(legacy.unresolved.len(), 1);
    assert_eq!(next.waived, legacy.waived);
    assert_eq!(next.excluded, legacy.excluded);
    assert_eq!(next.unresolved, legacy.unresolved);
    assert_eq!(next.covered, revision(1));
    assert_eq!(
        store
            .automation_receipt(CONSUMER, &prior.batch.id)
            .unwrap()
            .unwrap(),
        prior_receipt
    );
    assert_eq!(next.reservations[0].batch.deliveries.len(), 5);
    assert_eq!(next.reservations[0].batch.from_exclusive, revision(16));
    let new_json: String = connection
        .lock()
        .unwrap()
        .query_row(
            "SELECT state_json FROM automation_consumers WHERE consumer=?1",
            [CONSUMER],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        serde_json::from_str::<OldRequiredEpoch>(&new_json).is_err(),
        "older automation readers require epoch:String and must refuse before forming a lossy writer snapshot"
    );
    let mut stale = legacy.clone();
    stale.generation += 1;
    assert!(
        !store.automation_commit(&legacy, &stale, None).unwrap(),
        "an already-read old snapshot is CAS fenced"
    );
    assert_eq!(store.automation_state(CONSUMER).unwrap().unwrap(), next);
    // An old remote client can receive diagnostic JSON with a string epoch,
    // discard unknown fields, and still hold the current generation. The
    // decoded snapshot fence must reject that lossy write too.
    let mut lossy = next.clone();
    lossy.independent_review = false;
    lossy.reservations.clear();
    let mut old_write = lossy.clone();
    old_write.generation += 1;
    assert!(!store.automation_commit(&lossy, &old_write, None).unwrap());
    assert_eq!(store.automation_state(CONSUMER).unwrap().unwrap(), next);
    let history = store.automation_recoveries(CONSUMER, 100).unwrap();
    assert_eq!(history[0].reissued.as_ref().unwrap().from_attempt, 2);
    assert_eq!(
        history[0].reissued.as_ref().unwrap().from_action_id,
        legacy.active.unwrap().reissue.unwrap().from_action_id
    );
}

#[test]
fn restart_keeps_out_of_order_receipt_and_concurrent_ticks_reserve_once() {
    const TEST: &str = "delivery_independent::restart_keeps_out_of_order_receipt_and_concurrent_ticks_reserve_once";
    if super::isolated(TEST) {
        return;
    }
    let path = std::env::current_dir().unwrap().join("automation.sqlite");
    let store = compose::automation_store(Store::open(&path).unwrap()).unwrap();
    let host = Arc::new(Host::new());
    host.independent.store(true, Ordering::SeqCst);
    let trigger = DeliveryTrigger {
        coverage: CoverageClass::LandedCodeReviewV1,
        ..trigger()
    };
    evaluate(store.as_ref(), &host, &trigger, true);
    host.page(0, 2);
    let a = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.page(2, 4);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let handles = (0..2)
            .map(|_| {
                let (store, host, barrier, trigger) = (
                    store.clone(),
                    host.clone(),
                    barrier.clone(),
                    trigger.clone(),
                );
                scope.spawn(move || {
                    barrier.wait();
                    evaluate_at(store.as_ref(), &host, &trigger, true, 0)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.join().unwrap();
        }
    });
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.reservations.len(), 1);
    assert_eq!(host.actions.lock().unwrap().len(), 2);
    let b = state.reservations[0].clone();
    host.page(4, 4);
    host.evidence(&b);
    let before = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let receipt = store
        .automation_receipt(CONSUMER, &b.batch.id)
        .unwrap()
        .unwrap();
    drop(store);
    let reopened = compose::automation_store(Store::open(&path).unwrap()).unwrap();
    assert_eq!(
        reopened.automation_state(CONSUMER).unwrap().unwrap(),
        before
    );
    assert_eq!(
        reopened
            .automation_receipt(CONSUMER, &b.batch.id)
            .unwrap()
            .unwrap(),
        receipt
    );
    // A compatible settings edit is adopted while A is still executing.
    let edited = DeliveryTrigger {
        max_items: 5,
        threshold: 5,
        ..trigger.clone()
    };
    delivery::evaluate(
        reopened.as_ref(),
        host.as_ref(),
        delivery::Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &edited,
            enabled: true,
            dry_run: false,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(
        reopened
            .automation_state(CONSUMER)
            .unwrap()
            .unwrap()
            .reservations,
        before.reservations
    );
    host.evidence(&a);
    let finished = delivery::evaluate(
        reopened.as_ref(),
        host.as_ref(),
        delivery::Evaluation {
            consumer: CONSUMER,
            epoch: "v2",
            trigger: &edited,
            enabled: true,
            dry_run: false,
            now: now(),
        },
    )
    .unwrap()
    .state
    .unwrap();
    assert_eq!(finished.covered, revision(4));
    assert_eq!(
        reopened
            .automation_receipt(CONSUMER, &b.batch.id)
            .unwrap()
            .unwrap(),
        receipt
    );
    assert_eq!(host.actions.lock().unwrap().len(), 2);
}

#[test]
fn failed_range_never_remints_and_reservation_capacity_bounds_new_work() {
    if super::isolated(
        "delivery_independent::failed_range_never_remints_and_reservation_capacity_bounds_new_work",
    ) {
        return;
    }
    let (store, host, mut trigger) = review_fixture();
    trigger.threshold = 1;
    trigger.max_items = 1;
    // Bounded deterministic source pages exercise capacity, not execution load.
    for n in 1..=MAX_DELIVERY_RESERVATIONS + 2 {
        host.page(n - 1, n);
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        assert_eq!(result.state.as_ref().unwrap().observed, revision(n));
        if n >= MAX_DELIVERY_RESERVATIONS {
            if n > MAX_DELIVERY_RESERVATIONS {
                assert_eq!(result.reason, "reservation_capacity");
            }
            assert_eq!(
                result.state.unwrap().delivery_attempts().count(),
                MAX_DELIVERY_RESERVATIONS
            );
        }
    }
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    let mut failed = state.clone();
    failed.generation += 1;
    failed.active.as_mut().unwrap().state = BatchState::Exhausted;
    failed.active.as_mut().unwrap().reason = Some("frozen range remains unpaid".into());
    assert!(store.automation_commit(&state, &failed, None).unwrap());
    host.page(MAX_DELIVERY_RESERVATIONS + 2, MAX_DELIVERY_RESERVATIONS + 2);
    let after = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(after.reason, "reservation_capacity");
    assert_eq!(
        host.actions.lock().unwrap().len(),
        MAX_DELIVERY_RESERVATIONS
    );
    assert_eq!(after.state.unwrap().unreserved_deliveries().count(), 2);
}
