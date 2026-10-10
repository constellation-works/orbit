//! What admission retains for an attempt is released exactly when a committed
//! checkpoint settles, exhausts or retires that attempt, and never while a
//! retry or a concurrent evaluator can still claim it [ORB-14164].

use super::*;

const CONSUMER: &str = "fixture/released-attempts";

/// How the fixture run answers once admitted.
#[derive(Clone, Copy, PartialEq)]
enum Run {
    Pending,
    Applies,
    Stops,
}

/// A source with one member whose host pins every admitted attempt in a
/// set standing in for the ref namespace, and unpins on release.
struct PinningHost<'a> {
    store: &'a dyn AutomationStoreBackend,
    run: Cell<Run>,
    /// Admission pins, then fails before acknowledging the run.
    admit_fails: Cell<bool>,
    /// The member's input went stale before acknowledgement.
    retired: Cell<bool>,
    /// Another evaluator commits while this one reads the run's outcome.
    race: Cell<bool>,
    pins: RefCell<BTreeSet<String>>,
    released: RefCell<Vec<String>>,
}

impl<'a> PinningHost<'a> {
    fn new(store: &'a dyn AutomationStoreBackend) -> Self {
        Self {
            store,
            run: Cell::new(Run::Pending),
            admit_fails: Cell::new(false),
            retired: Cell::new(false),
            race: Cell::new(false),
            pins: RefCell::new(BTreeSet::new()),
            released: RefCell::new(Vec::new()),
        }
    }
}

fn member() -> StateMember {
    StateMember {
        key: "fixture-member".into(),
        task_ids: vec!["fixture-task".into()],
        fingerprint: "fixture-input".into(),
        source: SourceRevision {
            commit: "fixture-head".into(),
            tree: "fixture-tree".into(),
        },
        evidence: serde_json::json!({}),
        first_seen: at("2026-01-01T00:00:00Z"),
        changed_at: at("2026-01-01T00:00:00Z"),
        crew: None,
    }
}

impl MemberHost for PinningHost<'_> {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("fixture-repo".into(), member().source))
    }
    fn observe(&self, _: Option<&str>, _: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: vec![member()],
            withheld: BTreeMap::new(),
            next: None,
        })
    }
    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(if self.retired.get() {
            MemberAdmission::Retire("material_changed".into())
        } else {
            MemberAdmission::Admit
        })
    }
    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }
    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }
    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.pins.borrow_mut().insert(attempt.id.clone());
        if self.admit_fails.get() {
            return Err(AutomationError::Deferred("fixture_submit_failed".into()));
        }
        Ok(format!("run-{}", attempt.attempt))
    }
    fn outcome(&self, attempt: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        if self.race.replace(false) {
            let old = self.store.automation_state(CONSUMER)?.unwrap();
            let mut next = old.clone();
            next.generation += 1;
            assert!(self.store.automation_commit(&old, &next, None)?);
        }
        let action_id = attempt.action_id.clone().unwrap_or_default();
        Ok(match self.run.get() {
            Run::Pending => MemberOutcome::Pending,
            Run::Stops => MemberOutcome::Failed("stopped_without_member_evidence".into()),
            Run::Applies => MemberOutcome::Settled(MemberBatchEvidence {
                applied: vec![MemberEvidence {
                    action_id: action_id.clone(),
                    attempt_id: attempt.id.clone(),
                    member_key: attempt.member.key.clone(),
                    input_fingerprint: attempt.member.fingerprint.clone(),
                    resulting_fingerprint: attempt.member.fingerprint.clone(),
                    ready: true,
                    result: serde_json::json!({"task_id": "fixture-task"}),
                }],
                failed: BTreeMap::new(),
                superseded: BTreeMap::new(),
                action_id,
                attempt_id: attempt.id.clone(),
            }),
        })
    }
    fn release(&self, attempt: &MemberAttempt) {
        assert!(
            self.store
                .automation_state(CONSUMER)
                .unwrap()
                .unwrap()
                .members
                .unwrap()
                .active
                .is_none_or(|active| active.id != attempt.id),
            "an attempt is released only after the checkpoint retiring it commits"
        );
        self.pins.borrow_mut().remove(&attempt.id);
        self.released.borrow_mut().push(attempt.id.clone());
    }
}

fn tick(host: &PinningHost<'_>, minute: i64) -> Result<String, AutomationError> {
    evaluate(
        host.store,
        host,
        MemberEvaluation {
            consumer: CONSUMER,
            epoch: "fixture-epoch",
            enabled: true,
            dry_run: false,
            now: at("2026-01-01T00:00:00Z") + Duration::minutes(minute),
            trigger: &StateTrigger {
                kind: StateTriggerKind::PreparationEligible,
                owner_machine: "fixture-host".into(),
                branch: "fixture-branch".into(),
                debounce_minutes: 2,
                max_wait_minutes: 10,
                max_items: 1,
                retries: 1,
                deadline_minutes: 30,
                batch_size: Some(1),
                eligibility: Default::default(),
                freshness: Default::default(),
            },
        },
    )
    .map(|diagnostic| diagnostic.reason)
}

fn active_id(store: &dyn AutomationStoreBackend) -> Option<String> {
    store
        .automation_state(CONSUMER)
        .unwrap()
        .unwrap()
        .members
        .unwrap()
        .active
        .map(|active| active.id)
}

/// Admit the fixture member, returning its attempt id.
fn fire(host: &PinningHost<'_>) -> String {
    assert_eq!(tick(host, 0).unwrap(), "debouncing");
    let fired = tick(host, 2);
    let id = active_id(host.store).unwrap();
    assert!(
        host.pins.borrow().contains(&id),
        "admission pinned {fired:?}"
    );
    id
}

#[test]
fn attempts_are_released_once_settled_exhausted_or_retired() {
    if isolated("released_attempts::attempts_are_released_once_settled_exhausted_or_retired") {
        return;
    }
    // Settled: the receipt's checkpoint releases the pin.
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = PinningHost::new(store.as_ref());
    let id = fire(&host);
    assert_eq!(tick(&host, 3).unwrap(), "batch_pending");
    assert!(host.released.borrow().is_empty(), "a pending run keeps it");
    host.run.set(Run::Applies);
    tick(&host, 4).unwrap();
    assert_eq!(*host.released.borrow(), [id]);
    assert!(host.pins.borrow().is_empty());

    // Failed: a retry reuses the attempt and its pin; exhaustion releases it.
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = PinningHost::new(store.as_ref());
    let id = fire(&host);
    host.run.set(Run::Stops);
    assert_eq!(tick(&host, 3).unwrap(), "retry_backoff");
    assert_eq!(active_id(host.store).as_ref(), Some(&id));
    assert!(
        host.released.borrow().is_empty(),
        "the retry still needs it"
    );
    assert_eq!(tick(&host, 9).unwrap(), "fired");
    assert_eq!(tick(&host, 10).unwrap(), "needs_attention");
    assert_eq!(*host.released.borrow(), [id]);
    assert!(host.pins.borrow().is_empty());

    // Superseded before acknowledgement: an expired claim is retired...
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = PinningHost::new(store.as_ref());
    host.admit_fails.set(true);
    let id = fire(&host);
    tick(&host, 40).unwrap();
    assert_eq!(*host.released.borrow(), [id]);
    assert!(host.pins.borrow().is_empty());

    // ...and so is one whose every member went stale.
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = PinningHost::new(store.as_ref());
    host.admit_fails.set(true);
    let id = fire(&host);
    host.retired.set(true);
    tick(&host, 3).unwrap();
    assert_eq!(*host.released.borrow(), [id]);
    assert!(host.pins.borrow().is_empty());
}

#[test]
fn an_attempt_survives_a_settlement_that_loses_the_generation_race() {
    if isolated(
        "released_attempts::an_attempt_survives_a_settlement_that_loses_the_generation_race",
    ) {
        return;
    }
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = PinningHost::new(store.as_ref());
    let id = fire(&host);
    host.run.set(Run::Applies);
    host.race.set(true);
    assert!(matches!(
        tick(&host, 3),
        Err(AutomationError::Deferred(reason)) if reason == "concurrent_evaluation"
    ));
    assert!(host.released.borrow().is_empty());
    assert!(
        host.pins.borrow().contains(&id),
        "the attempt is still active"
    );

    tick(&host, 4).unwrap();
    assert_eq!(*host.released.borrow(), [id]);
}
