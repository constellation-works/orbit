use crate::{
    AutomationError,
    delivery::{self, ActionOutcome, DeliveryHost, Evaluation, evidence::EvidenceFacts},
};
use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
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
        evidence_reference: format!("https://github.com/owner/repo/pull/{n}"),
        evidence_digest: format!("evidence-{n}"),
        landed_at: now(),
    }
}

pub(super) struct Host {
    pub(super) page: Mutex<SourcePage>,
    pub(super) actions: Mutex<BTreeMap<String, String>>,
    evidence: Mutex<Option<CoverageEvidence>>,
    fail_admit: AtomicBool,
    pub(super) failed: AtomicBool,
    admission_deferred: AtomicBool,
    fail_head: AtomicBool,
    head_calls: AtomicUsize,
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
        Ok(match &*self.evidence.lock().unwrap() {
            None => ActionOutcome::Pending,
            Some(e) => {
                let bytes = serde_json::to_vec(e).unwrap();
                ActionOutcome::Evidence(EvidenceFacts {
                    artifact_digest: delivery::digest(&bytes),
                    bytes,
                    reference: "task:action/artifacts/automation-coverage.json".into(),
                    submitted_by: "authorized-run".into(),
                    authorized: true,
                    source_verified: true,
                })
            }
        })
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
fn frozen_batch_receipt_once_and_later_arrivals_pending() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let batch = first.active.unwrap();
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    host.page(2, 3);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.as_ref().unwrap().batch, batch.batch);
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.pending.len(), 3);
    host.evidence(&batch);
    host.page(3, 3);
    let accepted = evaluate(store.as_ref(), &host, &trigger, true);
    let state = accepted.state.unwrap();
    assert_eq!(state.covered, revision(2));
    assert_eq!(
        state
            .pending
            .iter()
            .map(|d| d.key.clone())
            .collect::<Vec<_>>(),
        vec![landing(3).key]
    );
    assert_eq!(accepted.receipts.len(), 1);
    // Artifact replacement cannot rewrite the receipt or reapply the old range.
    host.evidence
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .examination_complete = false;
    let replay = evaluate(store.as_ref(), &host, &trigger, true);
    assert_eq!(replay.receipts, accepted.receipts);
    assert_eq!(replay.state.unwrap().covered, revision(2));
    assert_eq!(host.actions.lock().unwrap().len(), 1);
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
fn retries_exhaust_frozen_budget_without_covering() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    let original = first.active.unwrap().batch;
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    let second = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(second.active.unwrap().attempt, 2);
    let admitted = delivery::evaluate(
        store.as_ref(),
        &host,
        Evaluation {
            consumer: "ws/qa",
            epoch: "v1",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: now() + chrono::Duration::minutes(6),
        },
    )
    .unwrap();
    assert_eq!(
        admitted.state.unwrap().active.unwrap().state,
        BatchState::Admitted
    );
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.as_ref().unwrap().state, BatchState::Exhausted);
    assert_eq!(state.active.unwrap().batch, original);
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.pending.len(), 2);
    assert_eq!(host.actions.lock().unwrap().len(), 2);
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
    };
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.authorized = true;
    facts.source_verified = false;
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.source_verified = true;
    facts.bytes = b"{}".to_vec();
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
}

#[test]
fn no_diff_and_unavailable_provider_do_not_count() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    {
        let mut p = host.page.lock().unwrap();
        p.deliveries[0].after.tree = p.deliveries[0].before.tree.clone();
        p.deliveries.pop();
        p.unresolved
            .insert("c2".into(), "provider unavailable".into());
    }
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert!(state.active.is_none());
    assert!(state.pending.is_empty());
    assert_eq!(state.pending_commits.len(), 2);
    assert_eq!(state.unresolved.len(), 1);
    assert_eq!(state.covered, revision(0));
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

/// A crash at the receipt transaction leaves both old C and the action replayable.
struct ReceiptFailure {
    inner: Arc<dyn AutomationStoreBackend>,
    fail: AtomicBool,
}

impl AutomationStoreBackend for ReceiptFailure {
    fn automation_state(&self, c: &str) -> Result<Option<AutomationState>, OrbitError> {
        self.inner.automation_state(c)
    }

    fn automation_initialize(&self, s: &AutomationState) -> Result<bool, OrbitError> {
        self.inner.automation_initialize(s)
    }

    fn automation_commit(
        &self,
        a: &AutomationState,
        b: &AutomationState,
        r: Option<&AcceptedCoverage>,
    ) -> Result<bool, OrbitError> {
        if r.is_some() && self.fail.swap(false, Ordering::SeqCst) {
            return Err(OrbitError::Store("injected receipt failure".into()));
        }
        self.inner.automation_commit(a, b, r)
    }

    fn automation_receipts(&self, c: &str, n: usize) -> Result<Vec<AcceptedCoverage>, OrbitError> {
        self.inner.automation_receipts(c, n)
    }
}

#[test]
fn receipt_failure_recovers_atomically() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    host.evidence(&state.active.unwrap());
    host.page(2, 2);
    let failed = ReceiptFailure {
        inner: store.clone(),
        fail: AtomicBool::new(true),
    };
    assert!(
        delivery::evaluate(
            &failed,
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
    assert_eq!(
        store.automation_state("ws/qa").unwrap().unwrap().covered,
        revision(0)
    );
    assert!(store.automation_receipts("ws/qa", 20).unwrap().is_empty());
    let recovered = evaluate(&failed, &host, &trigger, true);
    assert_eq!(recovered.state.unwrap().covered, revision(2));
    assert_eq!(recovered.receipts.len(), 1);
}

#[test]
fn waiver_settles_threshold_debt_without_manufacturing_coverage() {
    let (store, host, mut trigger) = setup();
    trigger.retries = 0;
    host.page(0, 2);
    let batch = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap()
        .batch;
    host.page(2, 2);
    host.failed.store(true, Ordering::SeqCst);
    let state = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(state.active.unwrap().state, BatchState::Exhausted);
    let request = WaiveBatchRequest {
        batch_id: batch.id,
        reason: "operator accepted scheduling debt".into(),
    };
    delivery::waive(store.as_ref(), "ws/qa", &request, "operator", now()).unwrap();
    delivery::waive(store.as_ref(), "ws/qa", &request, "operator", now()).unwrap();
    let state = store.automation_state("ws/qa").unwrap().unwrap();
    assert_eq!(state.covered, revision(0));
    assert_eq!(state.waived.len(), 2);
    assert!(state.pending.is_empty());
    assert_eq!(state.pending_commits.len(), 2);
    assert_eq!(store.automation_waivers("ws/qa", 20).unwrap().len(), 1);
    assert!(store.automation_receipts("ws/qa", 20).unwrap().is_empty());
    host.failed.store(false, Ordering::SeqCst);
    host.page(2, 4);
    let next = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(next.batch.deliveries.len(), 2);
    assert_eq!(
        next.batch.commits.len(),
        4,
        "waived code remains an examination obligation"
    );
    host.evidence(&next);
    host.page(4, 4);
    let covered = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap();
    assert_eq!(covered.covered, revision(4));
    assert!(covered.waived.is_empty());
}
