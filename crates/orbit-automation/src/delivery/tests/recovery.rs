//! Recovery keeps every obligation and never substitutes authorization for
//! evidence [ORB-12295].

use super::evidence::{Host, evaluate, now, setup};
use crate::{
    AutomationError,
    delivery::{self, Evaluation, recovery},
};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{RecoveryRequest, refusal};
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
