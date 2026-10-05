//! A settings-only edit is adopted by the evaluator itself with every
//! obligation retained, and every other edit still holds the consumer and
//! names why [ORB-14033].

use super::evidence::{Host, evaluate, landing, now, revision, setup};
use crate::delivery::{self, DEFINITION_CHANGED, Evaluation};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::members::MemberState;
use orbit_types::workflow::automation::recovery::{AutomationStall, SYSTEM_ACTOR, refusal};
use orbit_types::workflow::automation::*;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

const CONSUMER: &str = "ws/qa";

fn tick(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    epoch: &str,
    dry_run: bool,
) -> AutomationDiagnostic {
    delivery::evaluate(
        store,
        host,
        Evaluation {
            consumer: CONSUMER,
            epoch,
            trigger,
            enabled: true,
            dry_run,
            now: now(),
        },
    )
    .unwrap()
}

/// Record a stall marker through its own fenced write.
fn stall(store: &dyn AutomationStoreBackend) {
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    let mut next = state.clone();
    next.generation += 1;
    next.stall = Some(AutomationStall {
        reason: "provider_identity_missing".into(),
        since: now(),
        escalated_at: None,
        friction_id: None,
        divergence: None,
    });
    assert!(store.automation_stall(&state, &next).unwrap());
}

/// A baseline an older binary or another consumer kind persisted, which this
/// evaluator never writes itself.
fn foreign_baseline(
    change: impl FnOnce(&mut AutomationState),
) -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let trigger = super::evidence::trigger();
    let mut state = AutomationState {
        members: None,
        consumer: CONSUMER.into(),
        epoch: "v1".into(),
        trigger: Some(trigger.clone()),
        repository: "owner/repo".into(),
        branch: trigger.branch.clone(),
        generation: 0,
        baseline: revision(0),
        observed: revision(0),
        covered: revision(0),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: Default::default(),
        associations: Default::default(),
        active: None,
        stall: None,
    };
    change(&mut state);
    assert!(store.automation_initialize(&state).unwrap());

    (store, Host::new(), trigger)
}

/// A review consumer holding every kind of debt: a covered batch and its
/// receipt, a waived batch, a pending landing, an excluded landing and an
/// unresolved commit.
fn indebted() -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    let trigger = DeliveryTrigger {
        coverage: CoverageClass::LandedCodeReviewV1,
        retries: 0,
        ..super::evidence::trigger()
    };
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true).reason,
        "baselined"
    );

    // Covered: landings 1 and 2, with their receipt.
    host.page(0, 2);
    let attempt = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.evidence(&attempt);
    host.page(2, 2);
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true)
            .state
            .unwrap()
            .covered,
        revision(2)
    );

    // Waived: landings 3 and 4, whose only attempt failed.
    *host.evidence.lock().unwrap() = None;
    host.page(2, 4);
    evaluate(store.as_ref(), &host, &trigger, true);
    host.failed.store(true, Ordering::SeqCst);
    host.page(4, 4);
    let exhausted = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(exhausted.state, BatchState::Exhausted);
    host.failed.store(false, Ordering::SeqCst);
    delivery::waive(
        store.as_ref(),
        CONSUMER,
        &WaiveBatchRequest {
            batch_id: exhausted.batch.id.clone(),
            reason: "the failed review is examined by hand".into(),
        },
        "operator",
        now(),
    )
    .unwrap();

    // Pending landing 5, excluded landing 6 and an unresolved commit 7.
    host.page(4, 7);
    {
        let mut page = host.page.lock().unwrap();
        page.deliveries.pop();
        page.unresolved
            .insert(revision(7).commit, "provider_identity_missing".into());
        page.exclusions.insert(
            landing(6).key,
            DeliveryExclusion {
                attempt_id: "certificate-attempt".into(),
                assurance: "reviewed".into(),
                task_meaning_digest: "meaning".into(),
                final_candidate_tree: revision(6).tree,
            },
        );
    }
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    host.page(7, 7);
    assert_eq!(state.covered, revision(2));
    assert_eq!(state.waived.len(), 2);
    assert_eq!(state.pending.len(), 1);
    assert_eq!(state.excluded.len(), 1);
    assert_eq!(state.unresolved.len(), 1);
    assert!(state.active.is_none());

    (store, host, trigger)
}

fn retuned(trigger: &DeliveryTrigger) -> DeliveryTrigger {
    DeliveryTrigger {
        threshold: 1,
        ..trigger.clone()
    }
}

/// The debt a recovery must carry across untouched.
fn debt(state: &AutomationState) -> impl PartialEq + std::fmt::Debug {
    (
        state.baseline.clone(),
        state.covered.clone(),
        state.observed.clone(),
        state.pending.clone(),
        state.pending_commits.clone(),
        state.unresolved.clone(),
        state.waived.clone(),
        state.excluded.clone(),
    )
}

fn capture_warnings<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    use std::io::{self, Write};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&buffer)))
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    (result, logs.lines().map(str::to_string).collect())
}

#[test]
fn a_settings_only_edit_is_adopted_in_the_same_pass_with_every_obligation_retained() {
    let (store, host, trigger) = indebted();
    let before = store.automation_state(CONSUMER).unwrap().unwrap();
    let receipts = store.automation_receipts(CONSUMER, 100).unwrap();
    assert_eq!(receipts.len(), 1);
    let retuned = retuned(&trigger);

    // Preview projects the adoption without writing or reporting anything.
    let preview = tick(store.as_ref(), &host, &retuned, "v2", true);
    assert_eq!(preview.reason, "threshold_reached");
    assert_eq!(store.automation_state(CONSUMER).unwrap().unwrap(), before);
    assert!(host.adoptions.lock().unwrap().is_empty());

    // The lowered threshold admits the retained landing in the same tick.
    let (adopted, warnings) =
        capture_warnings(|| tick(store.as_ref(), &host, &retuned, "v2", false));
    assert_eq!(adopted.reason, "threshold_reached");
    assert!(adopted.refusals.is_empty());
    let after = adopted.state.unwrap();
    assert_eq!(after.epoch, "v2");
    assert_eq!(after.trigger.as_ref(), Some(&retuned));
    assert_eq!(debt(&after), debt(&before));
    assert_eq!(store.automation_receipts(CONSUMER, 100).unwrap(), receipts);
    let admitted = after.active.unwrap();
    assert_eq!(admitted.batch.epoch, "v2");
    assert_eq!(admitted.state, BatchState::Admitted);

    let recoveries = store.automation_recoveries(CONSUMER, 10).unwrap();
    assert_eq!(recoveries.len(), 1);
    let record = &recoveries[0];
    assert_eq!(record.by, SYSTEM_ACTOR);
    assert!(record.adopted_settings);
    assert!(record.reissued.is_none());
    assert_eq!(
        (record.previous_epoch.as_str(), record.epoch.as_str()),
        ("v1", "v2")
    );
    assert_eq!(record.previous_trigger.as_ref(), Some(&trigger));
    assert_eq!(record.trigger.as_ref(), Some(&retuned));
    assert!(record.reason.contains("(threshold)"), "{}", record.reason);

    let adopted_warnings = warnings
        .iter()
        .filter(|line| line.contains("WARN") && line.contains("adopted automatically"))
        .collect::<Vec<_>>();
    assert_eq!(adopted_warnings.len(), 1, "{warnings:?}");
    assert!(
        adopted_warnings[0].contains("qa: settings changed (threshold)"),
        "{}",
        adopted_warnings[0]
    );
    assert_eq!(
        *host.adoptions.lock().unwrap(),
        vec!["v1 -> v2: threshold".to_string()]
    );

    // Later ticks evaluate the adopted identity: nothing repeats.
    let (_, warnings) = capture_warnings(|| {
        tick(store.as_ref(), &host, &retuned, "v2", false);
        tick(store.as_ref(), &host, &retuned, "v2", false);
    });
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(host.adoptions.lock().unwrap().len(), 1);
    assert_eq!(store.automation_recoveries(CONSUMER, 10).unwrap().len(), 1);
}

/// Evaluate `trigger` under a new epoch and require the consumer to stay
/// exactly where it was, held at `definition_changed` for `expected`.
fn assert_held(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    expected: &str,
) {
    let before = store.automation_state(CONSUMER).unwrap();
    for dry_run in [true, false] {
        let held = tick(store, host, trigger, "v2", dry_run);
        assert_eq!(held.reason, DEFINITION_CHANGED, "{expected}");
        assert!(
            held.refusals.iter().any(|reason| reason == expected),
            "{:?} lacks {expected}",
            held.refusals
        );
    }
    assert_eq!(store.automation_state(CONSUMER).unwrap(), before);
    assert!(
        store
            .automation_recoveries(CONSUMER, 10)
            .unwrap()
            .is_empty()
    );
    assert!(host.adoptions.lock().unwrap().is_empty());
}

/// One edit to a configured trigger.
type Edit = fn(&mut DeliveryTrigger);

#[test]
fn edits_that_change_what_the_debt_means_are_never_adopted() {
    let edits: [(&str, Edit); 3] = [
        (refusal::BRANCH_CHANGED, |trigger| {
            trigger.branch = "main".into()
        }),
        (refusal::OWNER_CHANGED, |trigger| {
            trigger.owner_machine = Some("another-machine".into())
        }),
        (refusal::COVERAGE_CHANGED, |trigger| {
            trigger.coverage = CoverageClass::IntegratedQaV1
        }),
    ];
    for (expected, edit) in edits {
        let (store, host, trigger) = indebted();
        let mut edited = retuned(&trigger);
        edit(&mut edited);
        assert_held(store.as_ref(), &host, &edited, expected);
    }

    let (store, host, trigger) = indebted();
    *host.repository.lock().unwrap() = "owner/moved".into();
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::REPOSITORY_CHANGED,
    );
}

#[test]
fn an_executing_or_unsettled_action_is_never_interrupted_by_adoption() {
    // An admitted action whose task is still open.
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let admitted = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(
        admitted.state.unwrap().active.unwrap().state,
        BatchState::Admitted
    );
    host.page(2, 2);
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::ACTIVE_EXECUTION,
    );

    // A claim that never reached admission.
    let (store, host, trigger) = setup();
    host.page(0, 2);
    host.fail_admit.store(true, Ordering::SeqCst);
    let _ = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: now(),
        },
    );
    let claimed = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(claimed.active.unwrap().state, BatchState::Claimed);
    host.page(2, 2);
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::ACTIVE_EXECUTION,
    );
}

#[test]
fn a_consumer_the_evaluator_cannot_judge_alone_is_left_for_an_operator() {
    // Legacy state baselined before its trigger was recorded.
    let (store, host, trigger) = foreign_baseline(|state| state.trigger = None);
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::COVERAGE_UNVERIFIABLE,
    );

    let (store, host, trigger) =
        foreign_baseline(|state| state.members = Some(MemberState::default()));
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::MEMBER_CONSUMER,
    );

    let (store, host, trigger) = indebted();
    stall(store.as_ref());
    assert_held(
        store.as_ref(),
        &host,
        &retuned(&trigger),
        refusal::CONSUMER_STALLED,
    );
}

#[test]
fn a_host_that_does_not_adopt_keeps_every_edit_for_an_operator() {
    let (store, host, trigger) = indebted();
    host.adopts_settings.store(false, Ordering::SeqCst);
    let before = store.automation_state(CONSUMER).unwrap();

    let held = tick(store.as_ref(), &host, &retuned(&trigger), "v2", false);
    assert_eq!(held.reason, DEFINITION_CHANGED);
    assert!(held.refusals.is_empty());
    assert_eq!(store.automation_state(CONSUMER).unwrap(), before);
    assert!(host.adoptions.lock().unwrap().is_empty());
}
