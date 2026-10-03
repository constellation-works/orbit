use super::evidence::{evaluate, landing, now, revision, setup};
use crate::delivery::{self, Evaluation};
use orbit_common::OrbitError;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn frozen_batch_receipt_once_and_later_arrivals_pending() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let batch = first.active.unwrap();
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    host.page(2, 3);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.as_ref().unwrap().batch, batch.batch);
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.pending.len(), 3);
    host.evidence(&batch);
    host.page(3, 3);
    let accepted = evaluate(store.as_ref(), &host, &trigger, true);
    let state = accepted.state.unwrap();
    assert_eq!(state.covered, revision(2));
    assert_eq!(
        state
            .pending
            .iter()
            .map(|d| d.key.clone())
            .collect::<Vec<_>>(),
        vec![landing(3).key]
    );
    assert_eq!(accepted.receipts.len(), 1);
    // Artifact replacement cannot rewrite the receipt or reapply the old range.
    host.evidence
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .examination_complete = false;
    let replay = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(replay.receipts, accepted.receipts);
    assert_eq!(replay.state.unwrap().covered, revision(2));
    assert_eq!(host.actions.lock().unwrap().len(), 1);
}

#[test]
fn retries_exhaust_frozen_budget_without_covering() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let original = first.active.unwrap().batch;
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    let second = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(second.active.unwrap().attempt, 2);
    let admitted = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::minutes(6),
        },
    )
    .unwrap();
    assert_eq!(
        admitted.state.unwrap().active.unwrap().state,
        BatchState::Admitted
    );
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Exhausted);
    assert_eq!(state.active.unwrap().batch, original);
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.pending.len(), 2);
    assert_eq!(host.actions.lock().unwrap().len(), 2);
}

/// A crash at the receipt transaction leaves both old C and the action replayable.
struct ReceiptFailure {
    inner: Arc<dyn AutomationStoreBackend>,
    fail: AtomicBool,
}

impl AutomationStoreBackend for ReceiptFailure {
    fn automation_state(&self, c: &str) -> Result<Option<AutomationState>, OrbitError> {
        self.inner.automation_state(c)
    }

    fn automation_initialize(&self, s: &AutomationState) -> Result<bool, OrbitError> {
        self.inner.automation_initialize(s)
    }

    fn automation_commit(
        &self,
        a: &AutomationState,
        b: &AutomationState,
        r: Option<&AcceptedCoverage>,
    ) -> Result<bool, OrbitError> {
        if r.is_some() && self.fail.swap(false, Ordering::SeqCst) {
            return Err(OrbitError::Store("injected receipt failure".into()));
        }
        self.inner.automation_commit(a, b, r)
    }

    fn automation_receipts(&self, c: &str, n: usize) -> Result<Vec<AcceptedCoverage>, OrbitError> {
        self.inner.automation_receipts(c, n)
    }
}

#[test]
fn receipt_failure_recovers_atomically() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    host.evidence(&state.active.unwrap());
    host.page(2, 2);
    let failed = ReceiptFailure {
        inner: store.clone(),
        fail: AtomicBool::new(true),
    };
    assert!(
        delivery::evaluate(
            &failed,
            &host,
            Evaluation {
                consumer: "ws/qa",
                epoch: "v1",
                trigger: &trigger,
                enabled: true,
                dry_run: false,
                now: now()
            }
        )
        .is_err()
    );
    assert_eq!(
        store.automation_state("ws/qa").unwrap().unwrap().covered,
        revision(0)
    );
    assert!(store.automation_receipts("ws/qa", 20).unwrap().is_empty());
    let recovered = evaluate(&failed, &host, &trigger, true);
    assert_eq!(recovered.state.unwrap().covered, revision(2));
    assert_eq!(recovered.receipts.len(), 1);
}
