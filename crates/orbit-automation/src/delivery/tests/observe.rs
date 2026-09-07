//! Before-PR exclusions inside the shared delivery evaluator [ORB-11333].

use super::evidence::{Host, evaluate, landing, now, revision, setup};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;
use std::sync::Arc;

fn exclusion(n: usize) -> DeliveryExclusion {
    DeliveryExclusion {
        attempt_id: format!("rvw-{n}"),
        assurance: "independent_review".into(),
        task_meaning_digest: format!("meaning-{n}"),
        final_candidate_tree: revision(n).tree,
    }
}

fn review_setup() -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let (store, host, mut trigger) = setup();
    trigger.coverage = CoverageClass::LandedCodeReviewV1;
    trigger.threshold = 3;
    trigger.max_items = 10;
    (store, host, trigger)
}

/// Six landings of which four carry accepted before-PR coverage leave two
/// review obligations; the next uncovered landing makes review due.
#[test]
fn excluded_landings_do_not_count_toward_a_review_threshold() {
    let (store, host, trigger) = review_setup();
    host.page(0, 6);
    for n in [1, 2, 4, 5] {
        host.page
            .lock()
            .unwrap()
            .exclusions
            .insert(landing(n).key, exclusion(n));
    }

    let diagnostic = evaluate(store.as_ref(), &host, &trigger, true);
    let state = diagnostic.state.unwrap();
    assert_eq!(diagnostic.reason, "not_due");
    assert_eq!(state.pending.len(), 2);
    assert_eq!(state.excluded.len(), 4);
    assert!(state.active.is_none());
    assert!(host.actions.lock().unwrap().is_empty());

    host.page(6, 7);
    let diagnostic = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(diagnostic.reason, "threshold_reached");
    let state = diagnostic.state.unwrap();
    let batch = &state.active.as_ref().unwrap().batch;
    assert_eq!(
        batch
            .deliveries
            .iter()
            .map(|d| d.key.clone())
            .collect::<Vec<_>>(),
        vec![landing(3).key, landing(6).key, landing(7).key]
    );
    assert_eq!(batch.exclusions.len(), 4);
    assert_eq!(
        batch.commits.len(),
        7,
        "the range keeps excluded commits as context"
    );
    let template = evidence_template(state.active.as_ref().unwrap());
    assert_eq!(template.examined_deliveries.len(), 3);
}

/// QA counts every landing: the same exclusions never reach a QA consumer.
#[test]
fn qa_consumers_ignore_before_pr_exclusions() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    for n in [1, 2] {
        host.page
            .lock()
            .unwrap()
            .exclusions
            .insert(landing(n).key, exclusion(n));
    }
    let diagnostic = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(diagnostic.reason, "threshold_reached");
    let state = diagnostic.state.unwrap();
    assert!(state.excluded.is_empty());
    assert_eq!(state.active.unwrap().batch.deliveries.len(), 2);
}

/// Accepted coverage retires excluded landings with the range that
/// contained them.
#[test]
fn accepted_coverage_retires_exclusions_with_their_range() {
    let (store, host, trigger) = review_setup();
    host.page(0, 4);
    host.page
        .lock()
        .unwrap()
        .exclusions
        .insert(landing(2).key, exclusion(2));
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let attempt = state.active.clone().unwrap();
    assert_eq!(attempt.batch.deliveries.len(), 3);
    assert_eq!(attempt.batch.exclusions.len(), 1);

    host.evidence(&attempt);
    host.page(4, 4);
    let accepted = evaluate(store.as_ref(), &host, &trigger, true);
    let state = accepted.state.unwrap();
    assert_eq!(state.covered, revision(4));
    assert!(state.excluded.is_empty());
    assert!(state.pending.is_empty());
    assert_eq!(accepted.receipts.len(), 1);
    let _ = now();
}

/// A failed sweep advances nothing: pending obligations, the covered cursor,
/// and the exclusions all stay exactly where they were.
#[test]
fn failed_sweeps_retain_exclusions_and_do_not_advance_coverage() {
    let (store, host, mut trigger) = review_setup();
    trigger.retries = 0;
    host.page(0, 4);
    host.page
        .lock()
        .unwrap()
        .exclusions
        .insert(landing(2).key, exclusion(2));
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert!(state.active.is_some());

    host.failed.store(true, std::sync::atomic::Ordering::SeqCst);
    host.page(4, 4);
    let failed = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(failed.reason, "needs_attention");
    let state = failed.state.unwrap();
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.excluded.len(), 1);
    assert_eq!(state.pending.len(), 3);
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Exhausted);
}

/// A late certificate cannot rewrite a landing already pending: exclusions
/// only apply when the landing is first observed.
#[test]
fn a_pending_landing_is_not_excluded_retroactively() {
    let (store, host, trigger) = review_setup();
    host.page(0, 1);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.pending.len(), 1);

    host.page(1, 1);
    host.page
        .lock()
        .unwrap()
        .exclusions
        .insert(landing(1).key, exclusion(1));
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.pending.len(), 1);
    assert!(state.excluded.is_empty());
}
