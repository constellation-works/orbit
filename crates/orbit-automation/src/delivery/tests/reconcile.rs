use super::evidence::{Host, evaluate, now, revision, setup};
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

fn evaluate_at(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    minutes: i64,
) -> AutomationDiagnostic {
    delivery::evaluate(
        store,
        host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::minutes(minutes),
        },
    )
    .unwrap()
}

/// A review task that closed with malformed coverage once held its consumer
/// `admitted` forever: the bytes could never change, so it never settled.
#[test]
fn a_stopped_action_with_malformed_evidence_settles_and_spends_its_retry_budget() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.page(2, 2);
    // The incident's shape: an object where the schema expects a string.
    *host.raw_evidence.lock().unwrap() = Some(br#"{"schema_version":1,"batch_id":{}}"#.to_vec());

    // While the task is open its executor can still re-put the file.
    let open = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(open.state, BatchState::Admitted);
    assert!(open.reason.unwrap().contains("malformed"));

    host.stopped.store(true, Ordering::SeqCst);
    let settled = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(settled.state, BatchState::Claimed);
    assert_eq!(settled.attempt, 2);
    assert!(settled.action_id.is_none());
    assert!(
        settled
            .reason
            .as_deref()
            .unwrap()
            .contains("invalid type: map, expected a string"),
        "the parse error is recorded: {:?}",
        settled.reason
    );

    // After the backoff the next fire admits a fresh action over the same debt.
    let retried = evaluate_at(store.as_ref(), &host, &trigger, 6)
        .state
        .unwrap();
    let attempt = retried.active.unwrap();
    assert_eq!(attempt.state, BatchState::Admitted);
    assert_ne!(attempt.action_id, first.action_id);
    assert_eq!(attempt.batch, first.batch);
    assert_eq!(retried.covered, revision(0));

    // The retry closes the same way: the budget is spent, nothing is covered.
    let exhausted = evaluate_at(store.as_ref(), &host, &trigger, 7);
    assert_eq!(exhausted.reason, "needs_attention");
    let state = exhausted.state.unwrap();
    assert_eq!(state.active.unwrap().state, BatchState::Exhausted);
    assert_eq!(state.covered, revision(0));
    assert!(exhausted.receipts.is_empty());
}
