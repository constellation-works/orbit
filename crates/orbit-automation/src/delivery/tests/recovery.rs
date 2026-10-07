//! Recovery keeps every obligation and never substitutes authorization for
//! evidence [ORB-12295].

use super::evidence::{Host, evaluate, now, revision, setup};
use crate::{
    AutomationError,
    delivery::{self, Evaluation, recovery, reset},
};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{
    RecoveryPreview, RecoveryRequest, ResetRequest, refusal,
};
use orbit_types::workflow::automation::*;
use std::sync::atomic::Ordering;

const CONSUMER: &str = "ws/qa";

fn request(adopt: bool, reissue: bool) -> RecoveryRequest {
    RecoveryRequest {
        adopt_settings: adopt,
        reissue_action: reissue,
        replay_history: false,
        reason: "tonight's retuning keeps the same QA contract".into(),
    }
}

fn recovery<'a>(
    trigger: &'a DeliveryTrigger,
    epoch: &'a str,
    request: &'a RecoveryRequest,
) -> recovery::Recovery<'a> {
    recovery::Recovery {
        consumer: CONSUMER,
        epoch,
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

/// Drive a consumer to a settled failed action over two retained landings,
/// exactly as an archived unevidenced task leaves it.
fn stalled() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
    CoverageBatch,
) {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let batch = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap()
        .batch;

    // Spend the frozen retry budget: the worker fails, the automatic retry is
    // admitted after its backoff, and that attempt fails too.
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    evaluate(store.as_ref(), &host, &trigger, true);
    delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::minutes(6),
        },
    )
    .unwrap();
    let settled = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(settled.state, BatchState::Exhausted);
    assert_eq!(settled.attempt, 2);
    host.failed.store(false, Ordering::SeqCst);

    (store, host, trigger, batch)
}

/// The settings the operator retuned tonight: a lower threshold over the same
/// branch, repository and examination contract.
fn retuned(trigger: &DeliveryTrigger) -> DeliveryTrigger {
    DeliveryTrigger {
        threshold: 1,
        max_wait_minutes: 30,
        ..trigger.clone()
    }
}

#[test]
fn incompatible_and_unauthorized_changes_are_refused_without_touching_state() {
    let (store, _host, trigger, _batch) = stalled();
    let before = store.automation_state(CONSUMER).unwrap();

    let mut branch = retuned(&trigger);
    branch.branch = "main".into();
    let mut coverage = retuned(&trigger);
    coverage.coverage = CoverageClass::LandedCodeReviewV1;
    let mut owner = retuned(&trigger);
    owner.owner_machine = Some("another-machine".into());

    let adopt = request(true, false);
    let unexplained = RecoveryRequest {
        reason: "   ".into(),
        ..request(true, false)
    };

    for (expected, trigger, epoch, request, repository) in [
        (refusal::BRANCH_CHANGED, &branch, "v2", &adopt, "owner/repo"),
        (
            refusal::COVERAGE_CHANGED,
            &coverage,
            "v2",
            &adopt,
            "owner/repo",
        ),
        (refusal::OWNER_CHANGED, &owner, "v2", &adopt, "owner/repo"),
        (
            refusal::REPOSITORY_CHANGED,
            &branch,
            "v2",
            &adopt,
            "owner/moved",
        ),
        (
            refusal::MISSING_AUTHORIZATION,
            &retuned(&trigger),
            "v2",
            &unexplained,
            "owner/repo",
        ),
        (
            refusal::SETTINGS_UNCHANGED,
            &trigger,
            "v1",
            &adopt,
            "owner/repo",
        ),
    ] {
        let mut recovery = recovery(trigger, epoch, request);
        recovery.repository = repository;

        let error = recovery::apply(store.as_ref(), &recovery).expect_err("refused");
        let AutomationError::Refused(reasons) = error else {
            panic!("expected a typed refusal, got {error}");
        };
        assert!(reasons.contains(expected), "{reasons} lacks {expected}");
        assert_eq!(store.automation_state(CONSUMER).unwrap(), before);
        assert!(
            store
                .automation_recoveries(CONSUMER, 10)
                .unwrap()
                .is_empty()
        );
    }
}

/// A consumer still holding an admitted action whose task closed with
/// malformed coverage — the state an evaluation never reached to settle.
fn wedged() -> (
    std::sync::Arc<dyn AutomationStoreBackend>,
    Host,
    DeliveryTrigger,
    AutomationState,
) {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    evaluate(store.as_ref(), &host, &trigger, true);
    *host.raw_evidence.lock().unwrap() = Some(br#"{"schema_version":1,"batch_id":{}}"#.to_vec());
    host.stopped.store(true, Ordering::SeqCst);
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Admitted);

    (store, host, trigger, state)
}

#[test]
fn a_minted_action_is_stopped_only_once_its_task_closed_without_acceptable_evidence() {
    for batch_state in [BatchState::Claimed, BatchState::Admitted] {
        let (_store, host, _trigger, mut state) = wedged();
        state.active.as_mut().unwrap().state = batch_state;
        let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(liveness.terminal);
        assert!(liveness.failed_without_evidence);

        // The executor can still replace malformed bytes while its task is open.
        host.stopped.store(false, Ordering::SeqCst);
        let open = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(!open.terminal);
        assert!(!open.failed_without_evidence);

        // Valid evidence on a closed task is settled by acceptance, not failure.
        host.stopped.store(true, Ordering::SeqCst);
        *host.raw_evidence.lock().unwrap() = None;
        host.evidence(state.active.as_ref().unwrap());
        let accepted = delivery::action_liveness(&host, &state, now()).unwrap();
        assert!(accepted.terminal);
        assert!(!accepted.failed_without_evidence);
    }
}

#[test]
fn recovery_reissues_an_admitted_action_whose_task_closed_without_evidence() {
    let (store, _host, trigger, state) = wedged();
    let reissue = request(false, true);

    let mut executing = recovery(&trigger, "v1", &reissue);
    let error = recovery::apply(store.as_ref(), &executing).expect_err("live action refused");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::ACTIVE_EXECUTION), "{reasons}");

    executing.action_terminal = true;
    executing.action_failed_without_evidence = true;
    let preview = recovery::preview(store.as_ref(), &executing).unwrap();
    assert_eq!(preview.reason, "needs_attention");
    assert!(preview.action.as_ref().unwrap().reissuable);

    let applied = recovery::apply(store.as_ref(), &executing).unwrap();
    assert_eq!(
        applied.applied,
        vec![RecoveryPreview::REISSUED_ACTION.to_string()]
    );
    let after = store.automation_state(CONSUMER).unwrap().unwrap();
    let attempt = after.active.unwrap();
    assert_eq!(attempt.state, BatchState::Claimed);
    assert_eq!(attempt.attempt, 2);
    assert_eq!(attempt.batch, state.active.unwrap().batch);
    assert_eq!(after.covered, revision(0));
}

/// Deterministic interleaving: admission changes the action after the host's
/// terminal probe and before recovery loads its checkpoint.
#[test]
fn a_terminal_proof_cannot_reissue_a_concurrently_admitted_replacement() {
    let (store, _host, trigger, state) = wedged();
    let reissue = request(false, true);
    let mut operation = recovery(&trigger, "v1", &reissue);
    operation.expected_generation = Some(state.generation);
    operation.action_terminal = true;
    operation.action_failed_without_evidence = true;

    let mut admitted = state.clone();
    admitted.generation += 1;
    let active = admitted.active.as_mut().unwrap();
    active.attempt += 1;
    active.action_key = format!("automation:{}:{}", active.batch.id, active.attempt);
    active.action_id = Some("replacement-action".into());
    assert!(store.automation_commit(&state, &admitted, None).unwrap());

    let error = recovery::apply(store.as_ref(), &operation).unwrap_err();
    assert!(
        matches!(error, AutomationError::Deferred(reason) if reason == "concurrent_evaluation")
    );
    assert_eq!(store.automation_state(CONSUMER).unwrap(), Some(admitted));
    assert!(
        store
            .automation_recoveries(CONSUMER, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn recovery_does_not_reissue_a_terminal_action_with_acceptable_evidence() {
    let (store, host, trigger, state) = wedged();
    *host.raw_evidence.lock().unwrap() = None;
    host.evidence(state.active.as_ref().unwrap());
    let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
    assert!(liveness.terminal);
    assert!(!liveness.failed_without_evidence);

    let reissue = request(false, true);
    let mut operation = recovery(&trigger, "v1", &reissue);
    operation.action_terminal = liveness.terminal;
    operation.action_failed_without_evidence = liveness.failed_without_evidence;
    let error =
        recovery::apply(store.as_ref(), &operation).expect_err("valid evidence is not reissued");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(!reasons.contains(refusal::ACTIVE_EXECUTION), "{reasons}");
    assert!(reasons.contains(refusal::NO_SETTLED_ACTION), "{reasons}");
}

#[test]
fn reset_forgets_a_consumer_whose_admitted_action_already_stopped() {
    let (store, _host, trigger, state) = wedged();
    let request = ResetRequest {
        reason: "the review task closed with malformed coverage".into(),
        force: false,
    };
    let mut operation = reset::Reset {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        baseline: revision(2),
        released_refs: vec![],
        action_terminal: false,
        action_failed_without_evidence: false,
    };

    let error = reset::apply(store.as_ref(), &operation).expect_err("live action refused");
    let AutomationError::Refused(reasons) = error else {
        panic!("expected a typed refusal, got {error}");
    };
    assert!(reasons.contains(refusal::ACTION_EXECUTING), "{reasons}");
    assert_eq!(store.automation_state(CONSUMER).unwrap(), Some(state));

    operation.action_terminal = true;
    operation.action_failed_without_evidence = true;
    let applied = reset::apply(store.as_ref(), &operation).unwrap();
    assert!(applied.applied);
    assert!(store.automation_state(CONSUMER).unwrap().is_none());
}

#[test]
fn reset_can_forget_a_terminal_action_with_acceptable_evidence() {
    let (store, host, trigger, state) = wedged();
    *host.raw_evidence.lock().unwrap() = None;
    host.evidence(state.active.as_ref().unwrap());
    let liveness = delivery::action_liveness(&host, &state, now()).unwrap();
    assert!(liveness.terminal);
    assert!(!liveness.failed_without_evidence);

    let request = ResetRequest {
        reason: "the review task has already closed".into(),
        force: false,
    };
    let operation = reset::Reset {
        consumer: CONSUMER,
        epoch: "v1",
        trigger: &trigger,
        host_refusal: None,
        request: &request,
        by: "operator",
        now: now(),
        baseline: revision(2),
        released_refs: vec![],
        action_terminal: liveness.terminal,
        action_failed_without_evidence: liveness.failed_without_evidence,
    };
    let applied = reset::apply(store.as_ref(), &operation).unwrap();
    assert!(applied.applied);
    assert!(!applied.action.unwrap().reissuable);
    assert!(store.automation_state(CONSUMER).unwrap().is_none());
}
