//! A member consumer's first evaluation and checkpoint fencing: a disabled
//! consumer without state never probes its branch, and the store refuses a
//! checkpoint that resets a frozen attempt's budget, replaces its acknowledged
//! action or drops a failure tombstone.

use chrono::{DateTime, Duration, Utc};
use orbit_automation::AutomationError;
use orbit_automation::members::{
    MemberAdmission, MemberEvaluation, MemberHost, MemberOutcome, MemberPage, evaluate,
};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::{
    AutomationDiagnostic, SourceRevision,
    members::{MemberAttempt, StateMember, StateTrigger, StateTriggerKind},
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

const CONSUMER: &str = "host/ws/routine/pilot";

struct Host {
    actions: RefCell<BTreeMap<String, String>>,
    failed: RefCell<bool>,
    /// Counts branch probes and refuses each one, when set.
    head_probes: RefCell<Option<usize>>,
}

impl Host {
    fn new() -> Self {
        Self {
            actions: RefCell::new(BTreeMap::new()),
            failed: RefCell::new(false),
            head_probes: RefCell::new(None),
        }
    }
}

impl MemberHost for Host {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        if let Some(probes) = self.head_probes.borrow_mut().as_mut() {
            *probes += 1;
            return Err(AutomationError::Deferred("head_probed".into()));
        }
        Ok((
            "repo".into(),
            SourceRevision {
                commit: "source".into(),
                tree: "tree".into(),
            },
        ))
    }

    fn observe(&self, _: Option<&str>, now: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: vec![StateMember {
                key: "task".into(),
                task_ids: vec!["task".into()],
                fingerprint: "input".into(),
                source: self.head("")?.1,
                evidence: serde_json::json!({}),
                first_seen: now,
                changed_at: now,
                crew: None,
            }],
            withheld: BTreeMap::new(),
            next: None,
        })
    }

    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(MemberAdmission::Admit)
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(self.actions.borrow().get(&attempt.action_key).cloned())
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        Ok(self
            .actions
            .borrow_mut()
            .entry(attempt.action_key.clone())
            .or_insert_with(|| format!("run-{}", attempt.attempt))
            .clone())
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        if *self.failed.borrow() {
            Ok(MemberOutcome::Failed("fixture_failure".into()))
        } else {
            Ok(MemberOutcome::Pending)
        }
    }
}

fn trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        owner_machine: "host".into(),
        branch: "agent-main".into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 30,
        batch_size: None,
        eligibility: Default::default(),
        freshness: Default::default(),
    }
}

fn evaluate_at(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    enabled: bool,
    dry_run: bool,
    minute: i64,
) -> Result<AutomationDiagnostic, AutomationError> {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: CONSUMER,
            epoch: "epoch",
            trigger: &trigger(),
            enabled,
            dry_run,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
}

fn tick(store: &dyn AutomationStoreBackend, host: &Host, minute: i64) -> AutomationDiagnostic {
    evaluate_at(store, host, true, false, minute).unwrap()
}

#[test]
fn disabled_consumer_without_state_never_probes_the_branch_in_either_mode() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    *host.head_probes.borrow_mut() = Some(0);

    for dry_run in [false, true] {
        let diagnostic = evaluate_at(store.as_ref(), &host, false, dry_run, 0).unwrap();
        assert_eq!(diagnostic.reason, "disabled", "dry_run={dry_run}");
        assert!(diagnostic.state.is_none(), "dry_run={dry_run}");
        assert_eq!(*host.head_probes.borrow(), Some(0), "dry_run={dry_run}");
        assert!(
            store.automation_state(CONSUMER).unwrap().is_none(),
            "dry_run={dry_run} must not create state"
        );
    }
}

#[test]
fn enabled_consumer_without_state_still_probes_the_branch() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    *host.head_probes.borrow_mut() = Some(0);

    for dry_run in [false, true] {
        assert!(evaluate_at(store.as_ref(), &host, true, dry_run, 0).is_err());
    }
    assert_eq!(*host.head_probes.borrow(), Some(2));
}

#[test]
fn checkpoint_rejects_budget_reset_ack_replacement_and_lost_failure_tombstone() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    tick(store.as_ref(), &host, 0);
    let state = tick(store.as_ref(), &host, 2).state.unwrap();
    for mutation in 0..3 {
        let mut next = state.clone();
        next.generation += 1;
        let active = next.members.as_mut().unwrap().active.as_mut().unwrap();
        match mutation {
            0 => active.deadline += Duration::minutes(30),
            1 => active.max_attempts += 1,
            _ => active.action_id = Some("replacement".into()),
        }
        assert!(store.automation_commit(&state, &next, None).is_err());
    }
    *host.failed.borrow_mut() = true;
    let failed = tick(store.as_ref(), &host, 40).state.unwrap();
    let mut erased = failed.clone();
    erased.generation += 1;
    erased.members.as_mut().unwrap().failed.clear();
    assert!(store.automation_commit(&failed, &erased, None).is_err());
}
