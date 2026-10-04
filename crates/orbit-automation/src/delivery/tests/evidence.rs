use crate::{
    AutomationError,
    delivery::{self, ActionOutcome, DeliveryHost, Evaluation, evidence::EvidenceFacts},
};
use chrono::{TimeZone, Utc};
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub(super) fn revision(n: usize) -> SourceRevision {
    SourceRevision {
        commit: format!("c{n}"),
        tree: format!("t{n}"),
    }
}

pub(super) fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 6, 0, 0, 0).unwrap()
}

pub(super) fn trigger() -> DeliveryTrigger {
    DeliveryTrigger {
        owner_machine: Some("fixture-machine".into()),
        branch: "agent-main".into(),
        threshold: 2,
        max_wait_minutes: 60,
        coverage: CoverageClass::IntegratedQaV1,
        max_items: 2,
        retries: 1,
    }
}

pub(super) fn landing(n: usize) -> Delivery {
    Delivery {
        key: format!("pr:owner/repo:{n}"),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        before: revision(n - 1),
        after: revision(n),
        commits: vec![revision(n).commit],
        task_ids: vec!["task-a".into(), "task-b".into()],
        unattributed: None,
        evidence_reference: format!("https://github.com/owner/repo/pull/{n}"),
        evidence_digest: format!("evidence-{n}"),
        landed_at: now(),
    }
}

pub(super) struct Host {
    pub(super) page: Mutex<SourcePage>,
    pub(super) actions: Mutex<BTreeMap<String, String>>,
    pub(super) evidence: Mutex<Option<CoverageEvidence>>,
    /// Raw artifact bytes submitted instead of `evidence`, such as a file
    /// written before the Store validated coverage on put.
    pub(super) raw_evidence: Mutex<Option<Vec<u8>>>,
    /// The admitted action's task is terminal.
    pub(super) stopped: AtomicBool,
    pub(super) fail_admit: AtomicBool,
    pub(super) failed: AtomicBool,
    pub(super) admission_deferred: AtomicBool,
    pub(super) fail_head: AtomicBool,
    pub(super) head_calls: AtomicUsize,
}

impl Host {
    pub(super) fn new() -> Self {
        Self {
            page: Mutex::new(SourcePage {
                from: revision(0),
                through: revision(0),
                commits: vec![],
                deliveries: vec![],
                unresolved: BTreeMap::new(),
                associations: Default::default(),
                exclusions: Default::default(),
                complete: true,
            }),
            actions: Mutex::new(BTreeMap::new()),
            evidence: Mutex::new(None),
            raw_evidence: Mutex::new(None),
            stopped: AtomicBool::new(false),
            fail_admit: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            admission_deferred: AtomicBool::new(false),
            fail_head: AtomicBool::new(false),
            head_calls: AtomicUsize::new(0),
        }
    }

    pub(super) fn page(&self, from: usize, to: usize) {
        let commits = (from + 1..=to).map(|n| revision(n).commit).collect();
        *self.page.lock().unwrap() = SourcePage {
            from: revision(from),
            through: revision(to),
            commits,
            deliveries: (from + 1..=to).map(landing).collect(),
            unresolved: BTreeMap::new(),
            associations: Default::default(),
            exclusions: Default::default(),
            complete: true,
        };
    }

    pub(super) fn evidence(&self, attempt: &BatchAttempt) {
        let batch = &attempt.batch;
        *self.evidence.lock().unwrap() = Some(CoverageEvidence {
            schema_version: 1,
            batch_id: batch.id.clone(),
            consumer: batch.consumer.clone(),
            epoch: batch.epoch.clone(),
            input_digest: attempt.input_digest.clone(),
            action_id: attempt.action_id.clone().unwrap(),
            attempt: attempt.attempt,
            coverage: batch.coverage,
            from_exclusive: batch.from_exclusive.clone(),
            through_inclusive: batch.through_inclusive.clone(),
            examined_commits: batch.commits.clone(),
            examined_deliveries: batch.deliveries.iter().map(|d| d.key.clone()).collect(),
            examination_complete: true,
            checks: vec![ExaminationCheck {
                subject: "complete captured range".into(),
                method: "cargo test".into(),
                observation: "all tests passed; finding recorded".into(),
            }],
            findings: vec!["A finding may remain open".into()],
        });
    }
}

impl DeliveryHost for Host {
    fn admission_deferral(&self) -> Result<Option<String>, AutomationError> {
        Ok(self
            .admission_deferred
            .load(Ordering::SeqCst)
            .then(|| "open_instance".into()))
    }

    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        self.head_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_head.load(Ordering::SeqCst) {
            return Err(AutomationError::Deferred("evidence_unavailable".into()));
        }
        Ok(("owner/repo".into(), revision(0)))
    }

    fn observe(&self, _: &str, _: &AutomationState) -> Result<SourcePage, AutomationError> {
        Ok(self.page.lock().unwrap().clone())
    }

    fn admit(&self, a: &BatchAttempt) -> Result<String, AutomationError> {
        let mut actions = self.actions.lock().unwrap();
        let len = actions.len();
        let id = actions
            .entry(a.action_key.clone())
            .or_insert_with(|| format!("action-{len}"))
            .clone();
        if self.fail_admit.swap(false, Ordering::SeqCst) {
            return Err(AutomationError::Deferred("crash_after_mint".into()));
        }
        Ok(id)
    }

    fn outcome(&self, _: &BatchAttempt) -> Result<ActionOutcome, AutomationError> {
        if self.failed.load(Ordering::SeqCst) {
            return Ok(ActionOutcome::Failed {
                retryable: true,
                reason: "worker failed".into(),
            });
        }
        let raw = self.raw_evidence.lock().unwrap().clone();
        let bytes = match (raw, &*self.evidence.lock().unwrap()) {
            (Some(bytes), _) => bytes,
            (None, Some(e)) => serde_json::to_vec(e).unwrap(),
            (None, None) => return Ok(ActionOutcome::Pending),
        };
        Ok(ActionOutcome::Evidence(EvidenceFacts {
            artifact_digest: delivery::digest(&bytes),
            bytes,
            reference: "task:action/artifacts/automation-coverage.json".into(),
            submitted_by: "authorized-run".into(),
            authorized: true,
            source_verified: true,
            action_stopped: self.stopped.load(Ordering::SeqCst),
        }))
    }
}

pub(super) fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    enabled: bool,
) -> AutomationDiagnostic {
    delivery::evaluate(
        store,
        host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger,
            enabled,
            dry_run: false,
            now: now(),
        },
    )
    .unwrap()
}

pub(super) fn setup() -> (Arc<dyn AutomationStoreBackend>, Host, DeliveryTrigger) {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    let trigger = trigger();
    assert_eq!(
        evaluate(store.as_ref(), &host, &trigger, true).reason,
        "baselined"
    );
    (store, host, trigger)
}

#[test]
fn adversarial_evidence_cannot_manufacture_coverage() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let attempt = state.active.unwrap();
    host.evidence(&attempt);
    let valid = host.evidence.lock().unwrap().clone().unwrap();
    let mut variants = vec![];
    let mut e = valid.clone();
    e.examination_complete = false;
    variants.push(e);
    let mut e = valid.clone();
    e.examined_commits.pop();
    variants.push(e);
    let mut e = valid.clone();
    e.action_id = "unrelated-action".into();
    variants.push(e);
    let mut e = valid.clone();
    e.attempt += 1;
    variants.push(e);
    let mut e = valid.clone();
    e.input_digest = "stale".into();
    variants.push(e);
    let mut e = valid.clone();
    e.coverage = CoverageClass::LandedCodeReviewV1;
    variants.push(e);
    let mut e = valid.clone();
    e.through_inclusive.tree = "wrong-tree".into();
    variants.push(e);
    let mut e = valid.clone();
    e.checks.clear();
    variants.push(e);
    host.page(2, 2);
    for e in variants {
        *host.evidence.lock().unwrap() = Some(e);
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        assert_eq!(result.state.unwrap().covered, revision(0));
        assert!(result.receipts.is_empty());
    }
    let bytes = serde_json::to_vec(&valid).unwrap();
    let mut facts = EvidenceFacts {
        artifact_digest: delivery::digest(&bytes),
        bytes,
        reference: "artifact".into(),
        submitted_by: "run".into(),
        authorized: false,
        source_verified: true,
        action_stopped: false,
    };
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.authorized = true;
    facts.source_verified = false;
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.source_verified = true;
    facts.bytes = b"{}".to_vec();
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
}
