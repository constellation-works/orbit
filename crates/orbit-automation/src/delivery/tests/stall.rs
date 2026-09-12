//! Classification of deferred reasons and the escalation window.
//!
//! The evaluator's contract with the tick is what these tests pin down: which
//! reasons keep the silent retry, which ones suspend the consumer, and how
//! often a suspended consumer is allowed to report itself.

use super::evidence::{now, revision, trigger};
use crate::{
    AutomationError,
    delivery::{
        self, ActionOutcome, DeliveryHost, Evaluation,
        stall::{StallReport, stalled_reason},
    },
};
use chrono::{DateTime, Duration, Utc};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const CONSUMER: &str = "ws/qa";

/// A host whose observation always defers with one fixed reason.
struct Deferring {
    reason: &'static str,
    window: u32,
    observations: AtomicUsize,
    reports: Mutex<Vec<String>>,
}

impl Deferring {
    fn new(reason: &'static str, window: u32) -> Self {
        Self {
            reason,
            window,
            observations: AtomicUsize::new(0),
            reports: Mutex::new(vec![]),
        }
    }

    fn reports(&self) -> Vec<String> {
        self.reports.lock().unwrap().clone()
    }
}

impl DeliveryHost for Deferring {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("owner/repo".into(), revision(0)))
    }

    fn observe(&self, _: &str, _: &AutomationState) -> Result<SourcePage, AutomationError> {
        self.observations.fetch_add(1, Ordering::SeqCst);
        Err(AutomationError::Deferred(self.reason.into()))
    }

    fn admit(&self, _: &BatchAttempt) -> Result<String, AutomationError> {
        unreachable!("a deferred observation never admits an action")
    }

    fn outcome(&self, _: &BatchAttempt) -> Result<ActionOutcome, AutomationError> {
        unreachable!("no action is ever admitted")
    }

    fn stall_window_minutes(&self) -> u32 {
        self.window
    }

    fn report_stall(&self, report: &StallReport<'_>) -> Result<Option<String>, AutomationError> {
        let mut reports = self.reports.lock().unwrap();
        reports.push(report.reason.to_string());

        Ok(Some(format!("FR-{}", reports.len())))
    }
}

fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &Deferring,
    trigger: &DeliveryTrigger,
    at: DateTime<Utc>,
) -> Result<AutomationDiagnostic, AutomationError> {
    delivery::evaluate(
        store,
        host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger,
            enabled: true,
            dry_run: false,
            now: at,
        },
    )
}

/// Baseline first: the pinning pass never observes, so the deferral only
/// starts on the second evaluation.
fn baselined(
    reason: &'static str,
    window: u32,
) -> (Arc<dyn AutomationStoreBackend>, Deferring, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Deferring::new(reason, window);
    let trigger = trigger();
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, now())
            .unwrap()
            .reason,
        "baselined"
    );

    (store, host, trigger)
}

#[test]
fn a_transient_reason_keeps_retrying_silently_however_long_it_persists() {
    let (store, host, trigger) = baselined("source_backpressure", 60);

    for elapsed in [0, 61, 1_000] {
        let error = evaluate(
            store.as_ref(),
            &host,
            &trigger,
            now() + Duration::minutes(elapsed),
        )
        .expect_err("a transient deferral is still reported to the caller");
        assert!(
            matches!(&error, AutomationError::Deferred(reason) if reason == "source_backpressure"),
            "{error}"
        );
    }

    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.stall, None, "no marker is written for a retry");
    assert_eq!(state.generation, 0, "and the consumer is never moved");
    assert!(host.reports().is_empty(), "nothing is escalated");
    assert_eq!(host.observations.load(Ordering::SeqCst), 3);
}

#[test]
fn a_stuck_reason_suspends_the_consumer_and_escalates_once_past_the_window() {
    let (store, host, trigger) = baselined("repository_changed", 60);

    let stalled = evaluate(store.as_ref(), &host, &trigger, now()).unwrap();
    assert_eq!(stalled.reason, stalled_reason("repository_changed"));
    let stall = stalled.state.unwrap().stall.unwrap();
    assert_eq!(stall.since, now());
    assert_eq!(stall.escalated_at, None);
    assert!(
        host.reports().is_empty(),
        "a fresh stall is not yet worth a friction"
    );

    // Ten further ticks inside the window: one stalled line each, no writes and
    // no second observation attempt.
    for tick in 1..=10 {
        let diagnostic = evaluate(
            store.as_ref(),
            &host,
            &trigger,
            now() + Duration::minutes(tick),
        )
        .unwrap();
        assert_eq!(diagnostic.reason, stalled_reason("repository_changed"));
    }
    assert_eq!(
        host.observations.load(Ordering::SeqCst),
        1,
        "a suspended consumer stops probing the source"
    );
    assert!(host.reports().is_empty());
    assert_eq!(
        store
            .automation_state(CONSUMER)
            .unwrap()
            .unwrap()
            .generation,
        1,
        "the marker is written exactly once"
    );

    // Past the window it is escalated, and stays escalated exactly once.
    let escalated = evaluate(
        store.as_ref(),
        &host,
        &trigger,
        now() + Duration::minutes(61),
    )
    .unwrap()
    .state
    .unwrap()
    .stall
    .unwrap();
    assert_eq!(
        escalated.since,
        now(),
        "the age is measured from first sight"
    );
    assert_eq!(escalated.escalated_at, Some(now() + Duration::minutes(61)));
    assert_eq!(escalated.friction_id.as_deref(), Some("FR-1"));
    assert_eq!(host.reports(), vec!["repository_changed".to_string()]);

    for elapsed in [62, 121, 1_000] {
        assert_eq!(
            evaluate(
                store.as_ref(),
                &host,
                &trigger,
                now() + Duration::minutes(elapsed)
            )
            .unwrap()
            .reason,
            stalled_reason("repository_changed")
        );
    }
    assert_eq!(
        host.reports(),
        vec!["repository_changed".to_string()],
        "a stalled consumer files one friction, not one per tick"
    );
}

#[test]
fn a_configured_window_replaces_the_default_hour() {
    let (store, host, trigger) = baselined("state_missing", 5);

    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, now())
            .unwrap()
            .reason,
        stalled_reason("state_missing")
    );
    assert!(host.reports().is_empty());

    assert_eq!(
        evaluate(
            store.as_ref(),
            &host,
            &trigger,
            now() + Duration::minutes(5)
        )
        .unwrap()
        .state
        .unwrap()
        .stall
        .unwrap()
        .escalated_at,
        Some(now() + Duration::minutes(5))
    );
    assert_eq!(host.reports(), vec!["state_missing".to_string()]);
}

#[test]
fn preview_reports_a_stall_without_writing_or_escalating() {
    let (store, host, trigger) = baselined("repository_changed", 60);
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, now())
            .unwrap()
            .reason,
        stalled_reason("repository_changed")
    );

    let before = store.automation_state(CONSUMER).unwrap().unwrap();
    let preview = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: true,
            now: now() + Duration::minutes(600),
        },
    )
    .unwrap();

    assert_eq!(preview.reason, stalled_reason("repository_changed"));
    assert_eq!(store.automation_state(CONSUMER).unwrap(), Some(before));
    assert!(host.reports().is_empty());
}
