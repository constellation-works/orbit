use super::evidence::{evaluate, now, revision, setup};
use crate::delivery;
use orbit_types::workflow::automation::*;
use std::sync::atomic::Ordering;

#[test]
fn waiver_settles_threshold_debt_without_manufacturing_coverage() {
    let (store, host, mut trigger) = setup();
    trigger.retries = 0;
    host.page(0, 2);
    let batch = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap()
        .batch;
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.unwrap().state, BatchState::Exhausted);
    let request = WaiveBatchRequest {
        batch_id: batch.id,
        reason: "operator accepted scheduling debt".into(),
    };
    delivery::waive(store.as_ref(), "ws/qa", &request, "operator", now()).unwrap();
    delivery::waive(store.as_ref(), "ws/qa", &request, "operator", now()).unwrap();
    let state = store.automation_state("ws/qa").unwrap().unwrap();
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.waived.len(), 2);
    assert!(state.pending.is_empty());
    assert_eq!(state.pending_commits.len(), 2);
    assert_eq!(store.automation_waivers("ws/qa", 20).unwrap().len(), 1);
    assert!(store.automation_receipts("ws/qa", 20).unwrap().is_empty());
    host.failed.store(false, Ordering::SeqCst);
    host.page(2, 4);
    let next = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(next.batch.deliveries.len(), 2);
    assert_eq!(
        next.batch.commits.len(),
        4,
        "waived code remains an examination obligation"
    );
    host.evidence(&next);
    host.page(4, 4);
    let covered = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(covered.covered, revision(4));
    assert!(covered.waived.is_empty());
}
