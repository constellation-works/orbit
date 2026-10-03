use super::evidence::{evaluate, now, revision, setup};
use crate::delivery::{self, Evaluation};
use orbit_common::OrbitError;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

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
