use super::evidence::{Host, evaluate, now, revision, setup, trigger};
use crate::{
    AutomationError,
    delivery::{self, Evaluation},
};
use orbit_store::{Store, compose};
use std::sync::{Arc, atomic::Ordering};

#[test]
fn disabled_preview_without_state_matches_real_pass_and_does_not_probe_git() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    host.fail_head.store(true, Ordering::SeqCst);
    let trigger = trigger();

    let preview = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: false,
            dry_run: true,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(preview.reason, "disabled");
    assert!(preview.state.is_none());
    assert_eq!(host.head_calls.load(Ordering::SeqCst), 0);
    assert!(store.automation_state("ws/qa").unwrap().is_none());

    let real = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: false,
            dry_run: false,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(real.reason, "disabled");
    assert!(real.state.is_none());
    assert_eq!(host.head_calls.load(Ordering::SeqCst), 0);
    assert!(store.automation_state("ws/qa").unwrap().is_none());

    host.fail_head.store(false, Ordering::SeqCst);
    let preview_with_head = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: false,
            dry_run: true,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(preview_with_head.reason, "disabled");
    assert_eq!(host.head_calls.load(Ordering::SeqCst), 0);
    assert!(store.automation_state("ws/qa").unwrap().is_none());
}

#[test]
fn enabled_preview_without_state_still_requires_branch_head() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    let trigger = trigger();

    let would_baseline = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: true,
            now: now(),
        },
    )
    .unwrap();
    assert_eq!(would_baseline.reason, "would_baseline");
    assert_eq!(host.head_calls.load(Ordering::SeqCst), 1);
    assert!(store.automation_state("ws/qa").unwrap().is_none());

    host.fail_head.store(true, Ordering::SeqCst);
    let error = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: true,
            now: now(),
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        AutomationError::Deferred(reason) if reason == "evidence_unavailable"
    ));
    assert_eq!(host.head_calls.load(Ordering::SeqCst), 2);
    assert!(store.automation_state("ws/qa").unwrap().is_none());
}

#[test]
fn preview_reports_admission_deferral_without_persisting_observation() {
    let (store, host, trigger) = setup();
    let before = store.automation_state("ws/qa").unwrap();
    host.page(0, 2);
    host.admission_deferred.store(true, Ordering::SeqCst);

    let diagnostic = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: true,
            now: now(),
        },
    )
    .unwrap();

    assert_eq!(diagnostic.reason, "open_instance");
    assert_eq!(store.automation_state("ws/qa").unwrap(), before);
    assert!(host.actions.lock().unwrap().is_empty());
}

#[test]
fn bounded_batch_leaves_excess_debt_and_disabled_reconciles() {
    let (store, host, trigger) = setup();
    host.page(0, 4);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let a = first.active.unwrap();
    assert_eq!(a.batch.deliveries.len(), 2);
    assert_eq!(a.batch.through_inclusive, revision(2));
    host.evidence(&a);
    let state = evaluate(store.as_ref(), &host, &trigger, false)
        .state
        .unwrap();
    assert_eq!(state.covered, revision(2));
    assert_eq!(state.pending.len(), 2);
    assert!(state.active.is_none());
    assert_eq!(host.actions.lock().unwrap().len(), 1);
}

#[test]
fn crash_after_mint_replays_same_action_key() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    host.fail_admit.store(true, Ordering::SeqCst);
    assert!(
        delivery::evaluate(
            store.as_ref(),
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
    let claim = store.automation_state("ws/qa").unwrap().unwrap();
    assert!(claim.active.unwrap().action_id.is_none());
    host.page(2, 2);
    let recovered = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(
        recovered
            .state
            .unwrap()
            .active
            .unwrap()
            .action_id
            .as_deref(),
        Some("action-0")
    );
    assert_eq!(host.actions.lock().unwrap().len(), 1);
}

#[test]
fn concurrent_evaluators_admit_one_action() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let host = Arc::new(host);
    std::thread::scope(|scope| {
        let handles = (0..8)
            .map(|_| {
                let store = store.clone();
                let host = host.clone();
                let trigger = &trigger;
                scope.spawn(move || {
                    delivery::evaluate(
                        store.as_ref(),
                        host.as_ref(),
                        Evaluation {
                            consumer: "ws/qa",
                            epoch: "v1",
                            trigger,
                            enabled: true,
                            dry_run: false,
                            now: now(),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.join().unwrap();
        }
    });
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    let state = store.automation_state("ws/qa").unwrap().unwrap();
    assert_eq!(state.pending.len(), 2);
    assert_eq!(state.covered, revision(0));
}
