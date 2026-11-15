#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "firing fixtures panic when the store, clock or subject setup fails instead of hiding a broken fixture"
)]

//! Condition firing through the public `evaluate_condition` boundary over a
//! real store: one admission per subject edge across lost replies, restarts
//! and concurrent evaluators, holds and continuity, re-arm, cooldown, caps,
//! enable baselines, stalls, chain depth, and owner-only admission.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_automation::condition::{
    ConditionDiagnostic, ConditionEvaluation, ConditionHost, ConditionObservation,
    SubjectObservation, Truth, condition_epoch, evaluate_condition,
};
use orbit_automation::{AutomationError, DeferredReason};
use orbit_common::test_env;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_store::{Store, compose};
use orbit_types::workflow::automation::{
    ConditionArm, ConditionFire, ConditionFireState, ConditionOrigin, ConditionState,
    ConditionTrigger,
};

const CONSUMER: &str = "auto-task:fixture";
const CADENCE_SECONDS: u64 = 60;

fn at(minute: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-30T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
        + Duration::minutes(minute)
}

fn trigger(extra: serde_json::Value) -> ConditionTrigger {
    let mut value = serde_json::json!({"scope": "task", "when": "task.status == 'done'"});
    for (key, field) in extra.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn memory_store() -> Arc<dyn AutomationStoreBackend> {
    compose::automation_store(Store::open_in_memory().unwrap()).unwrap()
}

fn subject(key: &str, when: Truth, revision: &str) -> SubjectObservation {
    SubjectObservation {
        key: key.to_string(),
        when,
        rearm: None,
        revision: revision.to_string(),
        captured: serde_json::json!({"key": key, "revision": revision}),
        origin: None,
    }
}

type ObserveHook = Box<dyn FnOnce()>;

/// A board of subjects and a keyed action ledger. `admit` is idempotent per
/// action key, as the real mint and job admission are.
#[derive(Default)]
struct Board {
    subjects: RefCell<Vec<SubjectObservation>>,
    ledger: RefCell<BTreeMap<String, String>>,
    admitted: RefCell<Vec<ConditionFire>>,
    /// Admissions that fail before recording anything (a crash before admit).
    fail_before: Cell<usize>,
    /// Admissions that record the action and then lose the reply.
    lose_reply: Cell<usize>,
    deferral: RefCell<Option<String>>,
    on_observe: RefCell<Option<ObserveHook>>,
}

impl Board {
    fn set(&self, subjects: Vec<SubjectObservation>) {
        *self.subjects.borrow_mut() = subjects;
    }

    fn admissions(&self) -> usize {
        self.ledger.borrow().len()
    }
}

impl ConditionHost for Board {
    fn observe(
        &self,
        _state: &ConditionState,
        _now: DateTime<Utc>,
    ) -> Result<ConditionObservation, AutomationError> {
        let hook = self.on_observe.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
        Ok(ConditionObservation {
            subjects: self.subjects.borrow().clone(),
            complete: true,
            lap_complete: true,
            lap_after: None,
        })
    }

    fn lookup(&self, fire: &ConditionFire) -> Result<Option<String>, AutomationError> {
        Ok(self.ledger.borrow().get(&fire.action_key).cloned())
    }

    fn admission_deferral(&self, _fire: &ConditionFire) -> Result<Option<String>, AutomationError> {
        Ok(self.deferral.borrow().clone())
    }

    fn admit(&self, fire: &ConditionFire) -> Result<String, AutomationError> {
        if self.fail_before.get() > 0 {
            self.fail_before.set(self.fail_before.get() - 1);
            return Err(AutomationError::Deferred(
                DeferredReason::ConcurrentEvaluation,
            ));
        }
        let mut ledger = self.ledger.borrow_mut();
        let next = format!("task-{}", ledger.len() + 1);
        let id = ledger
            .entry(fire.action_key.clone())
            .or_insert(next)
            .clone();
        self.admitted.borrow_mut().push(fire.clone());
        if self.lose_reply.get() > 0 {
            self.lose_reply.set(self.lose_reply.get() - 1);
            return Err(AutomationError::Deferred(
                DeferredReason::ConcurrentEvaluation,
            ));
        }
        Ok(id)
    }
}

struct Pass<'a> {
    trigger: &'a ConditionTrigger,
    enabled: bool,
    refusal: Option<&'static str>,
    dry_run: bool,
}

impl<'a> Pass<'a> {
    fn live(trigger: &'a ConditionTrigger) -> Self {
        Self {
            trigger,
            enabled: true,
            refusal: None,
            dry_run: false,
        }
    }

    fn run(
        &self,
        store: &dyn AutomationStoreBackend,
        host: &dyn ConditionHost,
        now: DateTime<Utc>,
    ) -> Result<ConditionDiagnostic, AutomationError> {
        let epoch = condition_epoch(self.trigger, &("mint", "fixture")).unwrap();
        evaluate_condition(
            store,
            host,
            ConditionEvaluation {
                consumer: CONSUMER,
                epoch: &epoch,
                trigger: self.trigger,
                enabled: self.enabled,
                refusal: self.refusal,
                dry_run: self.dry_run,
                now,
                cadence_seconds: CADENCE_SECONDS,
            },
        )
    }
}

fn pass(
    store: &dyn AutomationStoreBackend,
    host: &Board,
    trigger: &ConditionTrigger,
    now: DateTime<Utc>,
) -> ConditionDiagnostic {
    Pass::live(trigger).run(store, host, now).unwrap()
}

#[test]
fn edges_admit_once_with_the_frozen_subject_and_rearm_under_a_new_identity() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({}));

    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    assert_eq!(pass(&*store, &board, &trigger, at(0)).reason, "baselined");

    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    let fired = pass(&*store, &board, &trigger, at(1));
    assert_eq!(fired.reason, "fired");
    assert_eq!(board.admissions(), 1);
    let first = board.admitted.borrow()[0].clone();
    assert_eq!(first.subject, "ORB-1");
    assert_eq!(first.revision, "2");
    assert_eq!(first.depth, 1);
    assert_eq!(first.captured["revision"], "2");
    assert_eq!(first.action_key, format!("condition:{}:0", first.fire_id));
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    let recorded = state.fire(&first.fire_id).unwrap();
    assert_eq!(recorded.state, ConditionFireState::Acknowledged);
    assert_eq!(recorded.action_id.as_deref(), Some("task-1"));

    // Still true: the subject stays fired and nothing is admitted again.
    board.set(vec![subject("ORB-1", Truth::True, "3")]);
    pass(&*store, &board, &trigger, at(2));
    assert_eq!(board.admissions(), 1, "a held truth is one edge, not many");

    // `rearm` defaults to !(when); the next true edge is a new fire.
    board.set(vec![subject("ORB-1", Truth::False, "4")]);
    pass(&*store, &board, &trigger, at(3));
    board.set(vec![subject("ORB-1", Truth::True, "4")]);
    pass(&*store, &board, &trigger, at(4));
    assert_eq!(board.admissions(), 2);
    let second = board.admitted.borrow()[1].clone();
    assert_ne!(
        second.fire_id, first.fire_id,
        "a re-armed edge has its own identity"
    );
}

#[test]
fn enable_baselines_true_subjects_unless_fire_existing_and_again_on_reenable() {
    let trigger_baseline = trigger(serde_json::json!({}));
    let store = memory_store();
    let board = Board::default();
    board.set(vec![subject("ORB-1", Truth::True, "1")]);
    assert_eq!(
        pass(&*store, &board, &trigger_baseline, at(0)).reason,
        "baselined"
    );
    pass(&*store, &board, &trigger_baseline, at(1));
    assert_eq!(board.admissions(), 0, "a baselined subject never fires");
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert!(state.subjects["ORB-1"].baselined);

    // Disable, then re-enable while the subject is still true: baseline again.
    let disabled = Pass {
        enabled: false,
        ..Pass::live(&trigger_baseline)
    };
    assert_eq!(
        disabled.run(&*store, &board, at(2)).unwrap().reason,
        "disabled"
    );
    board.set(vec![subject("ORB-1", Truth::False, "2")]);
    disabled.run(&*store, &board, at(3)).unwrap();
    board.set(vec![subject("ORB-1", Truth::True, "3")]);
    assert_eq!(
        pass(&*store, &board, &trigger_baseline, at(4)).reason,
        "baselined"
    );
    assert_eq!(board.admissions(), 0, "re-enable baselines too");

    let fire_existing = trigger(serde_json::json!({"on_enable": "fire_existing"}));
    let store = memory_store();
    let board = Board::default();
    board.set(vec![subject("ORB-1", Truth::True, "1")]);
    assert_eq!(pass(&*store, &board, &fire_existing, at(0)).reason, "fired");
    assert_eq!(board.admissions(), 1);
}

#[test]
fn a_lost_reply_or_a_crash_before_admission_finishes_once_after_restart() {
    let dir = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = dir.path().join("condition.db");
    let trigger = trigger(serde_json::json!({}));
    let board = Board::default();

    {
        let store = compose::automation_store(Store::open(&path).unwrap()).unwrap();
        board.set(vec![
            subject("ORB-1", Truth::False, "1"),
            subject("ORB-2", Truth::False, "1"),
        ]);
        pass(&*store, &board, &trigger, at(0));
        board.set(vec![
            subject("ORB-1", Truth::True, "2"),
            subject("ORB-2", Truth::True, "2"),
        ]);
        // One admission fails before anything is recorded (a crash before
        // admit); the other records its action and loses the reply.
        board.lose_reply.set(1);
        board.fail_before.set(1);
        pass(&*store, &board, &trigger, at(1));
        let state = store.condition_state(CONSUMER).unwrap().unwrap();
        assert!(
            state
                .fires
                .iter()
                .all(|fire| fire.state == ConditionFireState::Intent && fire.attempts == 1),
            "both edges stay recorded intents: {:?}",
            state.fires
        );
    }

    // Restart: a new store handle over the same file settles both intents.
    let store = compose::automation_store(Store::open(&path).unwrap()).unwrap();
    pass(&*store, &board, &trigger, at(2));
    assert_eq!(board.admissions(), 2, "each edge admits exactly one action");
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert!(
        state
            .fires
            .iter()
            .all(|fire| fire.state == ConditionFireState::Acknowledged)
    );
    let keys: Vec<_> = board
        .admitted
        .borrow()
        .iter()
        .map(|fire| fire.action_key.clone())
        .collect();
    let distinct: BTreeSet<_> = keys.iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "the lost-reply retry was answered by lookup, not a second admit: {keys:?}"
    );
}

#[test]
fn an_admission_that_keeps_failing_stops_after_its_attempt_budget() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({}));
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    board.fail_before.set(usize::MAX);
    for minute in 1..=8 {
        pass(&*store, &board, &trigger, at(minute));
    }
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.fires.len(), 1);
    assert_eq!(state.fires[0].state, ConditionFireState::Failed);
    assert_eq!(state.fires[0].attempts, 5);
    assert_eq!(board.admissions(), 0);
}

#[test]
fn a_waiting_admission_keeps_its_key_and_spends_no_attempt() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({}));
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    *board.deferral.borrow_mut() = Some("open instance task-0".into());
    for minute in 1..=7 {
        pass(&*store, &board, &trigger, at(minute));
    }
    let fire = store.condition_state(CONSUMER).unwrap().unwrap().fires[0].clone();
    assert_eq!(fire.state, ConditionFireState::Intent);
    assert_eq!(fire.attempts, 0);
    *board.deferral.borrow_mut() = None;
    pass(&*store, &board, &trigger, at(8));
    assert_eq!(board.admissions(), 1);
    assert_eq!(board.admitted.borrow()[0].fire_id, fire.fire_id);
}

#[test]
fn of_two_concurrent_evaluators_only_one_records_and_admits_the_edge() {
    let store = memory_store();
    let board = Rc::new(Board::default());
    let trigger = trigger(serde_json::json!({}));
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);

    // The second evaluator loads the same generation, and the first runs a
    // whole pass while the second is still observing.
    let inner_store = Arc::clone(&store);
    let inner_board = Rc::clone(&board);
    let inner_trigger = trigger.clone();
    *board.on_observe.borrow_mut() = Some(Box::new(move || {
        let fired = pass(&*inner_store, &inner_board, &inner_trigger, at(1));
        assert_eq!(fired.reason, "fired");
    }));
    let loser = Pass::live(&trigger).run(&*store, &*board, at(1));
    assert!(
        matches!(
            loser,
            Err(AutomationError::Deferred(
                DeferredReason::ConcurrentEvaluation
            ))
        ),
        "{loser:?}"
    );
    assert_eq!(board.admissions(), 1);
    pass(&*store, &board, &trigger, at(2));
    assert_eq!(
        board.admissions(),
        1,
        "the losing pass wrote nothing to retry"
    );
    assert_eq!(
        store
            .condition_state(CONSUMER)
            .unwrap()
            .unwrap()
            .fires
            .len(),
        1
    );
}

#[test]
fn holds_need_continuous_truth_and_unknown_neither_fires_nor_rearms() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({"for_minutes": 10}));
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));

    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    for minute in [1, 5, 9] {
        assert_eq!(
            pass(&*store, &board, &trigger, at(minute)).reason,
            "holding"
        );
    }
    // A gap longer than the continuity limit (5 minutes at a 1-minute
    // cadence) restarts the hold: 16 is 15 minutes after the first truth.
    assert_eq!(pass(&*store, &board, &trigger, at(16)).reason, "holding");
    assert_eq!(board.admissions(), 0, "a broken hold never fires");
    for minute in [20, 24] {
        pass(&*store, &board, &trigger, at(minute));
    }
    assert_eq!(board.admissions(), 0);
    pass(&*store, &board, &trigger, at(26));
    assert_eq!(board.admissions(), 1, "ten continuous minutes fire");

    // An unknown rearm keeps the subject fired; a later truth is no new edge.
    board.set(vec![subject(
        "ORB-1",
        Truth::Unknown("tasks['ORB-9'] missing".into()),
        "3",
    )]);
    pass(&*store, &board, &trigger, at(27));
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.subjects["ORB-1"].arm, ConditionArm::Fired);
    board.set(vec![subject("ORB-1", Truth::True, "3")]);
    for minute in 28..40 {
        pass(&*store, &board, &trigger, at(minute));
    }
    assert_eq!(board.admissions(), 1);

    // An armed subject that is only ever unknown never fires.
    let store = memory_store();
    let board = Board::default();
    let instant = trigger_with_defaults();
    board.set(vec![subject("ORB-2", Truth::False, "1")]);
    pass(&*store, &board, &instant, at(0));
    board.set(vec![subject(
        "ORB-2",
        Truth::Unknown("unreadable".into()),
        "2",
    )]);
    for minute in 1..10 {
        pass(&*store, &board, &instant, at(minute));
    }
    assert_eq!(board.admissions(), 0);
}

fn trigger_with_defaults() -> ConditionTrigger {
    trigger(serde_json::json!({}))
}

#[test]
fn cooldown_and_fire_caps_defer_edges_oldest_first_without_dropping_them() {
    let store = memory_store();
    let board = Board::default();
    let capped = trigger(serde_json::json!({"max_fires_per_hour": 1, "max_fires_per_day": 1}));
    board.set(vec![
        subject("ORB-1", Truth::False, "1"),
        subject("ORB-2", Truth::False, "1"),
    ]);
    pass(&*store, &board, &capped, at(0));
    board.set(vec![
        subject("ORB-1", Truth::True, "2"),
        subject("ORB-2", Truth::True, "2"),
    ]);
    assert_eq!(pass(&*store, &board, &capped, at(1)).reason, "fired");
    assert_eq!(board.admissions(), 1);
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    let waiting = if state.subjects["ORB-1"].arm == ConditionArm::Fired {
        "ORB-2"
    } else {
        "ORB-1"
    };
    assert_eq!(
        state.subjects[waiting].deferred.as_deref(),
        Some("fire_cap")
    );
    // Keep observing (inside the continuity limit) until the daily cap opens.
    let mut minute = 2;
    while minute < 24 * 60 {
        pass(&*store, &board, &capped, at(minute));
        minute += 4;
    }
    assert_eq!(board.admissions(), 1, "the cap defers, it never bursts");
    pass(&*store, &board, &capped, at(24 * 60 + 2));
    assert_eq!(
        board.admissions(),
        2,
        "the deferred edge fires once allowed"
    );
    assert_eq!(board.admitted.borrow()[1].subject, waiting);

    let store = memory_store();
    let board = Board::default();
    let cooled = trigger(serde_json::json!({"cooldown_minutes": 30}));
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &cooled, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    pass(&*store, &board, &cooled, at(1));
    board.set(vec![subject("ORB-1", Truth::False, "3")]);
    pass(&*store, &board, &cooled, at(2));
    board.set(vec![subject("ORB-1", Truth::True, "4")]);
    for minute in [3, 7, 11, 15, 19, 23, 27] {
        pass(&*store, &board, &cooled, at(minute));
    }
    assert_eq!(board.admissions(), 1, "inside the cooldown nothing fires");
    pass(&*store, &board, &cooled, at(31));
    assert_eq!(board.admissions(), 2);
}

#[test]
fn excess_matches_at_enable_refuse_the_enable_and_a_storm_stalls_the_pass() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({"max_matches": 2}));
    let three = |when: Truth| {
        (1..=3)
            .map(|n| subject(&format!("ORB-{n}"), when.clone(), "1"))
            .collect::<Vec<_>>()
    };
    board.set(three(Truth::True));
    let refused = pass(&*store, &board, &trigger, at(0));
    assert_eq!(refused.reason, "too_many_matches");
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert!(!state.enabled, "the enable did not take");
    assert_eq!(state.stall.as_ref().unwrap().sample.len(), 3);
    assert_eq!(
        pass(&*store, &board, &trigger, at(1)).reason,
        "too_many_matches",
        "a stall holds until an operator clears it"
    );
    assert_eq!(board.admissions(), 0);

    let store = memory_store();
    let board = Board::default();
    board.set(three(Truth::False));
    pass(&*store, &board, &trigger, at(0));
    board.set(three(Truth::True));
    assert_eq!(pass(&*store, &board, &trigger, at(1)).reason, "match_storm");
    assert_eq!(board.admissions(), 0);
}

#[test]
fn a_chain_deeper_than_five_is_refused_and_recorded_and_a_flapping_subject_stalls() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger_with_defaults();
    let with_origin = |key: &str, when: Truth, depth: u32| SubjectObservation {
        origin: Some(ConditionOrigin {
            trigger: "auto-task:upstream".into(),
            fire_id: format!("upstream-{depth}"),
            depth,
            subject: "workspace".into(),
        }),
        ..subject(key, when, "1")
    };
    board.set(vec![
        with_origin("ORB-4", Truth::False, 4),
        with_origin("ORB-5", Truth::False, 5),
    ]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![
        with_origin("ORB-4", Truth::True, 4),
        with_origin("ORB-5", Truth::True, 5),
    ]);
    let refused = pass(&*store, &board, &trigger, at(1));
    assert_eq!(refused.reason, "loop_suspected");
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    let refusal = state
        .fires
        .iter()
        .find(|fire| fire.subject == "ORB-5")
        .unwrap();
    assert_eq!(refusal.state, ConditionFireState::Refused);
    assert_eq!(refusal.depth, 6);
    assert_eq!(state.stall.as_ref().unwrap().reason, "loop_suspected");
    assert!(
        board
            .admitted
            .borrow()
            .iter()
            .all(|fire| fire.subject != "ORB-5" && fire.depth <= 5),
        "a depth-6 fire is never admitted"
    );

    let store = memory_store();
    let board = Board::default();
    board.set(vec![subject("ORB-1", Truth::False, "0")]);
    pass(&*store, &board, &trigger, at(0));
    let mut minute = 1;
    let mut last = String::new();
    for flap in 0..4 {
        board.set(vec![subject("ORB-1", Truth::True, &format!("t{flap}"))]);
        last = pass(&*store, &board, &trigger, at(minute)).reason;
        board.set(vec![subject("ORB-1", Truth::False, &format!("f{flap}"))]);
        pass(&*store, &board, &trigger, at(minute + 1));
        minute += 2;
    }
    assert_eq!(last, "oscillating");
    assert_eq!(board.admissions(), 3);
}

#[test]
fn followers_dry_runs_and_disabled_passes_admit_nothing_and_keep_pending_intents() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger_with_defaults();
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    board.fail_before.set(1);
    pass(&*store, &board, &trigger, at(1));
    let pending = store.condition_state(CONSUMER).unwrap().unwrap();
    assert_eq!(pending.fires[0].state, ConditionFireState::Intent);

    for refusal in [
        "owned_elsewhere",
        "replica_checkout",
        "ownership_unresolved",
    ] {
        let follower = Pass {
            refusal: Some(refusal),
            ..Pass::live(&trigger)
        };
        assert_eq!(
            follower.run(&*store, &board, at(2)).unwrap().reason,
            refusal
        );
    }
    assert_eq!(board.admissions(), 0, "only the owner finishes its intents");
    assert_eq!(store.condition_state(CONSUMER).unwrap().unwrap(), pending);

    let preview = Pass {
        dry_run: true,
        ..Pass::live(&trigger)
    };
    board.set(vec![
        subject("ORB-1", Truth::True, "2"),
        subject("ORB-2", Truth::True, "1"),
    ]);
    let dry = preview.run(&*store, &board, at(3)).unwrap();
    assert_eq!(dry.would_fire, vec!["ORB-2".to_string()]);
    assert_eq!(board.admissions(), 0, "a dry run admits nothing");
    assert_eq!(store.condition_state(CONSUMER).unwrap().unwrap(), pending);
}

#[test]
fn a_recorded_fire_cannot_be_rewritten_by_a_later_commit() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger_with_defaults();
    board.set(vec![subject("ORB-1", Truth::False, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::True, "2")]);
    pass(&*store, &board, &trigger, at(1));
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    let mut forged = state.clone();
    forged.generation += 1;
    forged.fires[0].captured = serde_json::json!({"key": "ORB-9"});
    assert!(
        !matches!(store.condition_commit(&state, &forged), Ok(true)),
        "a fire's captured subject is immutable"
    );
    let mut stale = state.clone();
    stale.generation += 2;
    assert!(!matches!(store.condition_commit(&state, &stale), Ok(true)));
    assert_eq!(store.condition_state(CONSUMER).unwrap().unwrap(), state);
}

/// Subjects split into laps the way the task prefilter pages them: each lap
/// ends at its last subject, and the next lap continues after it.
#[derive(Default)]
struct Paged {
    laps: Vec<Vec<SubjectObservation>>,
    admitted: Cell<usize>,
}

impl ConditionHost for Paged {
    fn observe(
        &self,
        state: &ConditionState,
        _now: DateTime<Utc>,
    ) -> Result<ConditionObservation, AutomationError> {
        let index = match state.lap_after.as_deref() {
            None => 0,
            Some(after) => self
                .laps
                .iter()
                .position(|lap| lap.last().is_some_and(|last| last.key == after))
                .map_or(0, |index| index + 1),
        };
        let lap = self.laps.get(index).cloned().unwrap_or_default();
        let last = index + 1 >= self.laps.len();
        Ok(ConditionObservation {
            lap_after: if last {
                None
            } else {
                lap.last().map(|subject| subject.key.clone())
            },
            subjects: lap,
            complete: true,
            lap_complete: last,
        })
    }

    fn lookup(&self, _fire: &ConditionFire) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn admit(&self, _fire: &ConditionFire) -> Result<String, AutomationError> {
        self.admitted.set(self.admitted.get() + 1);
        Ok(format!("task-{}", self.admitted.get()))
    }
}

#[test]
fn a_paged_enable_baselines_true_subjects_on_every_page_not_only_the_first() {
    let store = memory_store();
    let host = Paged {
        laps: vec![
            vec![subject("ORB-1", Truth::False, "1")],
            vec![subject("ORB-2", Truth::True, "1")],
        ],
        ..Paged::default()
    };
    let trigger = trigger(serde_json::json!({}));
    assert_eq!(
        Pass::live(&trigger)
            .run(&*store, &host, at(0))
            .unwrap()
            .reason,
        "baselined",
        "the enable reads every page before it baselines"
    );
    Pass::live(&trigger).run(&*store, &host, at(1)).unwrap();
    assert_eq!(
        host.admitted.get(),
        0,
        "a true subject on page two never fires"
    );
}

#[test]
fn a_paged_enable_counts_matches_on_every_page_against_max_matches() {
    let store = memory_store();
    let host = Paged {
        laps: vec![
            vec![subject("ORB-1", Truth::False, "1")],
            vec![
                subject("ORB-2", Truth::True, "1"),
                subject("ORB-3", Truth::True, "1"),
                subject("ORB-4", Truth::True, "1"),
            ],
        ],
        ..Paged::default()
    };
    let trigger = trigger(serde_json::json!({"max_matches": 2}));
    let diagnostic = Pass::live(&trigger).run(&*store, &host, at(0)).unwrap();
    assert_eq!(
        diagnostic.reason, "too_many_matches",
        "three matches on page two refuse an enable with max_matches 2"
    );
    assert_eq!(host.admitted.get(), 0);
}

#[test]
fn a_pending_intent_from_the_old_definition_is_refused_not_admitted_under_the_edit() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({}));
    board.set(vec![subject("ORB-1", Truth::True, "1")]);
    pass(&*store, &board, &trigger, at(0));
    board.set(vec![subject("ORB-1", Truth::False, "2")]);
    pass(&*store, &board, &trigger, at(1));
    board.set(vec![subject("ORB-1", Truth::True, "3")]);
    *board.deferral.borrow_mut() = Some("open_instance task-9".into());
    pass(&*store, &board, &trigger, at(2));
    assert_eq!(
        board.admissions(),
        0,
        "the intent waits on its open instance"
    );

    // The definition is edited (a new epoch) before the waiting intent admits.
    *board.deferral.borrow_mut() = None;
    let edited = condition_epoch(&trigger, &("mint", "edited")).unwrap();
    evaluate_condition(
        &*store,
        &board,
        ConditionEvaluation {
            consumer: CONSUMER,
            epoch: &edited,
            trigger: &trigger,
            enabled: true,
            refusal: None,
            dry_run: false,
            now: at(3),
            cadence_seconds: CADENCE_SECONDS,
        },
    )
    .unwrap();
    assert_eq!(
        board.admissions(),
        0,
        "an edge recorded under the old definition is never admitted under the edited one"
    );
    let state = store.condition_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.fires[0].state, ConditionFireState::Refused);
}

#[test]
fn capacity_baseline_omits_false_subjects_and_finds_later_edge() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(serde_json::json!({}));
    board.set(
        (0..1001)
            .map(|i| subject(&format!("task-{i}"), Truth::False, "1"))
            .collect(),
    );
    pass(&*store, &board, &trigger, at(0));
    assert!(
        store
            .condition_state(CONSUMER)
            .unwrap()
            .unwrap()
            .subjects
            .len()
            <= 1000
    );
    board.set(vec![subject("task-1000", Truth::True, "2")]);
    pass(&*store, &board, &trigger, at(1));
    assert_eq!(board.admissions(), 1);
}

#[test]
fn capacity_subjects_backpressure_without_losing_edges() {
    let store = memory_store();
    let board = Board::default();
    let trigger = trigger(
        serde_json::json!({"max_matches": 1000, "max_fires_per_hour": 1000, "max_fires_per_day": 1000}),
    );
    let epoch = condition_epoch(&trigger, &("mint", "fixture")).unwrap();
    let mut state = ConditionState::new(CONSUMER, &epoch);
    state.enabled = true;
    for i in 0..995 {
        state.subjects.insert(
            format!("old-{i}"),
            orbit_types::workflow::automation::ConditionSubject {
                arm: ConditionArm::Fired,
                observed_at: Some(at(0)),
                ..Default::default()
            },
        );
    }
    assert!(store.condition_initialize(&state).unwrap());
    let mut subjects = (0..995)
        .map(|i| subject(&format!("old-{i}"), Truth::True, "1"))
        .collect::<Vec<_>>();
    subjects.extend((0..20).map(|i| subject(&format!("new-{i}"), Truth::True, "1")));
    board.set(subjects);
    let result = pass(&*store, &board, &trigger, at(1));
    let saved = result.state.unwrap();
    assert!(saved.subjects.len() <= 1000);
    assert!(
        saved
            .skip_counts
            .get("backpressure")
            .copied()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(board.admissions(), 5);
    board.set(
        (0..20)
            .map(|i| subject(&format!("new-{i}"), Truth::True, "1"))
            .collect(),
    );
    pass(&*store, &board, &trigger, at(31 * 24 * 60));
    pass(&*store, &board, &trigger, at(31 * 24 * 60 + 1));
    assert_eq!(
        board.admissions(),
        20,
        "edges deferred by capacity are eventually admitted"
    );
}

#[test]
fn capacity_fires_preserves_pending_intents_and_makes_progress() {
    let store = memory_store();
    let board = Board::default();
    let trigger =
        trigger(serde_json::json!({"max_fires_per_hour": 1000, "max_fires_per_day": 1000}));
    let epoch = condition_epoch(&trigger, &("mint", "fixture")).unwrap();
    let mut state = ConditionState::new(CONSUMER, &epoch);
    state.enabled = true;
    for i in 0..999 {
        state.fires.push(ConditionFire {
            fire_id: format!("old-{i}"),
            subject: format!("old-{i}"),
            revision: "1".into(),
            arm_sequence: 0,
            epoch: epoch.clone(),
            observed_at: at(-2880),
            depth: 1,
            parent_fire_id: None,
            captured: serde_json::json!({}),
            action_key: format!("old-{i}"),
            state: if i == 0 {
                ConditionFireState::Intent
            } else {
                ConditionFireState::Acknowledged
            },
            action_id: None,
            acknowledged_at: Some(at(-2880)),
            attempts: 0,
            detail: None,
        });
    }
    assert!(store.condition_initialize(&state).unwrap());
    *board.deferral.borrow_mut() = Some("waiting".into());
    board.set(vec![
        subject("new-1", Truth::True, "1"),
        subject("new-2", Truth::True, "1"),
    ]);
    let saved = pass(&*store, &board, &trigger, at(0)).state.unwrap();
    assert!(saved.fires.len() <= 1000);
    assert_eq!(
        saved.fire("old-0").unwrap().state,
        ConditionFireState::Intent
    );
    assert_eq!(
        saved
            .fires
            .iter()
            .filter(|f| f.subject.starts_with("new-"))
            .count(),
        2
    );

    // Saturation with only unsettled intents must defer the edge, not drop
    // an intent or mark the subject fired.
    let full_store = memory_store();
    let mut full = ConditionState::new(CONSUMER, &epoch);
    full.enabled = true;
    full.fires = (0..1000)
        .map(|i| {
            let mut fire = saved.fire("old-0").unwrap().clone();
            fire.fire_id = format!("pending-{i}");
            fire.action_key = fire.fire_id.clone();
            fire
        })
        .collect();
    assert!(full_store.condition_initialize(&full).unwrap());
    board.set(vec![subject("waiting", Truth::True, "1")]);
    let result = pass(&*full_store, &board, &trigger, at(1));
    assert_eq!(result.reason, "backpressure");
    let saved = result.state.unwrap();
    assert_eq!(saved.fires.len(), 1000);
    assert!(
        saved
            .fires
            .iter()
            .all(|fire| fire.state == ConditionFireState::Intent)
    );
    assert_eq!(
        saved.subjects["waiting"].deferred.as_deref(),
        Some("backpressure")
    );
}
