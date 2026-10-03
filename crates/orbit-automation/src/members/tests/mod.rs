use super::*;

use chrono::Duration;
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use std::{cell::RefCell, collections::BTreeSet};

mod reconcile;

struct Host {
    fingerprint: RefCell<String>,
    actions: RefCell<BTreeMap<String, String>>,
    evidence: RefCell<Option<MemberEvidence>>,
    failed: RefCell<bool>,
    deferral: RefCell<Option<String>>,
    lose_ack: RefCell<bool>,
}

impl Host {
    fn new() -> Self {
        Self {
            fingerprint: RefCell::new("input".into()),
            actions: RefCell::new(BTreeMap::new()),
            evidence: RefCell::new(None),
            failed: RefCell::new(false),
            deferral: RefCell::new(None),
            lose_ack: RefCell::new(false),
        }
    }
}

impl MemberHost for Host {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
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
                fingerprint: self.fingerprint.borrow().clone(),
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
        Ok(self
            .deferral
            .borrow()
            .clone()
            .map(MemberAdmission::Withhold)
            .unwrap_or(MemberAdmission::Admit))
    }

    fn observable(&self, keys: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
        Ok(keys.clone())
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(self.actions.borrow().get(&attempt.action_key).cloned())
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        let id = self
            .actions
            .borrow_mut()
            .entry(attempt.action_key.clone())
            .or_insert_with(|| format!("run-{}", attempt.attempt))
            .clone();
        if *self.lose_ack.borrow() {
            return Err(AutomationError::Deferred("lost_acknowledgement".into()));
        }
        Ok(id)
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        if let Some(evidence) = self.evidence.borrow().clone() {
            Ok(MemberOutcome::Settled(MemberBatchEvidence {
                action_id: evidence.action_id.clone(),
                attempt_id: evidence.attempt_id.clone(),
                applied: vec![evidence],
                failed: BTreeMap::new(),
            }))
        } else if *self.failed.borrow() {
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
        eligibility: PreparationEligibility::default(),
        freshness: Default::default(),
    }
}

fn tick(store: &dyn AutomationStoreBackend, host: &Host, minute: i64) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}
