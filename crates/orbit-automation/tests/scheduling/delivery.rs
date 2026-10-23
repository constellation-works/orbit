//! The delivery evaluator through `delivery::evaluate` against an in-memory
//! automation store and a scriptable [`DeliveryHost`]: one action per
//! concurrent admission, evidence that cannot manufacture coverage, receipt
//! failures that recover atomically and malformed evidence that settles.
//! `delivery_adopt` and `delivery_recovery` share this fixture.

use chrono::{TimeZone, Utc};
use orbit_automation::AutomationError;
use orbit_automation::delivery::{
    self, ActionOutcome, DeliveryHost, Evaluation, evidence::EvidenceFacts,
};
use orbit_common::OrbitError;
use orbit_store::{Store, compose, contracts::AutomationStoreBackend};
use orbit_types::workflow::automation::*;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) const CONSUMER: &str = "ws/qa";

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

/// The paths each fixture delivery's diff changes: one source file and a
/// lockfile that complete evidence skips with a reason.
fn changed_paths(delivery: &Delivery) -> Vec<String> {
    vec![
        format!("src/{}.rs", delivery.after.commit),
        "Cargo.lock".into(),
    ]
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
    /// Repository identity the configured branch resolves to.
    pub(super) repository: Mutex<String>,
    /// The host adopts compatible definition edits, as auto-tasks do.
    pub(super) adopts_settings: AtomicBool,
    /// Each automatic adoption reported, as `previous -> epoch: changes`.
    pub(super) adoptions: Mutex<Vec<String>>,
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
                lookup_retries: Default::default(),
                exclusions: Default::default(),
                complete: true,
            }),
            actions: Mutex::new(BTreeMap::new()),
            evidence: Mutex::new(None),
            raw_evidence: Mutex::new(None),
            stopped: AtomicBool::new(false),
            fail_admit: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            repository: Mutex::new("owner/repo".into()),
            adopts_settings: AtomicBool::new(true),
            adoptions: Mutex::new(vec![]),
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
            lookup_retries: Default::default(),
            exclusions: Default::default(),
            complete: true,
        };
    }

    pub(super) fn evidence(&self, attempt: &BatchAttempt) {
        let batch = &attempt.batch;
        *self.evidence.lock().unwrap() = Some(CoverageEvidence {
            schema_version: COVERAGE_EVIDENCE_SCHEMA_VERSION,
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
            delivery_examinations: batch
                .deliveries
                .iter()
                .map(|d| DeliveryExamination {
                    delivery: d.key.clone(),
                    examined_paths: vec![format!("src/{}.rs", d.after.commit)],
                    skipped_paths: vec![SkippedPath {
                        path: "Cargo.lock".into(),
                        reason: "generated lockfile".into(),
                    }],
                    verdict: DeliveryVerdict::Clean,
                    rationale: format!(
                        "{} keeps its error paths typed and its tests exercise them",
                        d.key
                    ),
                })
                .collect(),
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
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok((self.repository.lock().unwrap().clone(), revision(0)))
    }

    fn adopts_settings(&self) -> bool {
        self.adopts_settings.load(Ordering::SeqCst)
    }

    fn report_adoption(
        &self,
        report: &delivery::adopt::AdoptionReport<'_>,
    ) -> Result<Option<String>, AutomationError> {
        let mut adoptions = self.adoptions.lock().unwrap();
        adoptions.push(format!(
            "{} -> {}: {}",
            report.previous_epoch,
            report.epoch,
            report.changes.join(", ")
        ));
        Ok(Some(format!("friction-{}", adoptions.len())))
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

    fn outcome(&self, attempt: &BatchAttempt) -> Result<ActionOutcome, AutomationError> {
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
            changed_paths: attempt
                .batch
                .deliveries
                .iter()
                .map(|d| (d.key.clone(), changed_paths(d)))
                .collect(),
        }))
    }
}

/// Evaluate the consumer under epoch `v1` at `now() + minutes`.
pub(super) fn evaluate_at(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    enabled: bool,
    minutes: i64,
) -> Result<AutomationDiagnostic, AutomationError> {
    delivery::evaluate(
        store,
        host,
        Evaluation {
            consumer: CONSUMER,
            epoch: "v1",
            trigger,
            enabled,
            dry_run: false,
            now: now() + chrono::Duration::minutes(minutes),
        },
    )
}

pub(super) fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    trigger: &DeliveryTrigger,
    enabled: bool,
) -> AutomationDiagnostic {
    evaluate_at(store, host, trigger, enabled, 0).unwrap()
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
                scope.spawn(move || evaluate_at(store.as_ref(), host.as_ref(), trigger, true, 0))
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let _ = handle.join().unwrap();
        }
    });
    assert_eq!(host.actions.lock().unwrap().len(), 1);
    let state = store.automation_state(CONSUMER).unwrap().unwrap();
    assert_eq!(state.pending.len(), 2);
    assert_eq!(state.covered, revision(0));
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
    // Each refusal names its typed reason on the attempt.
    let mut typed = vec![];
    // A stamp: the commit and delivery lists alone, as version 1 accepted.
    let mut e = valid.clone();
    e.schema_version = 1;
    e.delivery_examinations.clear();
    typed.push(("unsupported_schema_version", e));
    let mut e = valid.clone();
    e.delivery_examinations.clear();
    typed.push(("missing_delivery_examination", e));
    let mut e = valid.clone();
    e.delivery_examinations.pop();
    typed.push(("missing_delivery_examination", e));
    let mut e = valid.clone();
    let duplicate = e.delivery_examinations[0].clone();
    e.delivery_examinations[1] = duplicate;
    typed.push(("duplicate_delivery_examination", e));
    // A changed source path neither examined nor skipped.
    let mut e = valid.clone();
    e.delivery_examinations[0].examined_paths.clear();
    typed.push(("examined_paths_mismatch", e));
    // A path outside the frozen diff.
    let mut e = valid.clone();
    e.delivery_examinations[0]
        .examined_paths
        .push("src/unrelated.rs".into());
    typed.push(("examined_paths_mismatch", e));
    let mut e = valid.clone();
    e.delivery_examinations[0].skipped_paths[0].reason = " ".into();
    typed.push(("skipped_path_without_reason", e));
    let mut e = valid.clone();
    e.delivery_examinations[0].verdict = DeliveryVerdict::Findings(vec![]);
    typed.push(("invalid_verdict", e));
    let mut e = valid.clone();
    e.delivery_examinations[0].rationale = "looks good".into();
    typed.push(("trivial_rationale", e));
    let mut e = valid.clone();
    let rationale = e.delivery_examinations[0].rationale.clone();
    e.delivery_examinations[1].rationale = rationale;
    typed.push(("trivial_rationale", e));
    host.page(2, 2);
    for e in variants {
        *host.evidence.lock().unwrap() = Some(e);
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        assert_eq!(result.state.unwrap().covered, revision(0));
        assert!(result.receipts.is_empty());
    }
    for (reason, e) in typed {
        *host.evidence.lock().unwrap() = Some(e);
        let result = evaluate(store.as_ref(), &host, &trigger, true);
        let state = result.state.unwrap();
        assert_eq!(state.covered, revision(0), "{reason}");
        assert!(result.receipts.is_empty(), "{reason}");
        let recorded = state.active.unwrap().reason.unwrap_or_default();
        assert!(recorded.ends_with(reason), "{reason}: {recorded}");
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
        changed_paths: attempt
            .batch
            .deliveries
            .iter()
            .map(|d| (d.key.clone(), changed_paths(d)))
            .collect(),
    };
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.authorized = true;
    facts.source_verified = false;
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.source_verified = true;
    let mut findings = valid.clone();
    findings.delivery_examinations[0].verdict = DeliveryVerdict::Findings(vec!["task-c".into()]);
    facts.bytes = serde_json::to_vec(&findings).unwrap();
    facts.artifact_digest = delivery::digest(&facts.bytes);
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_ok());
    facts.changed_paths.clear();
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
    facts.bytes = b"{}".to_vec();
    assert!(delivery::evidence::validate(&attempt, &facts, now()).is_err());
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
    assert!(evaluate_at(&failed, &host, &trigger, true, 0).is_err());
    assert_eq!(
        store.automation_state(CONSUMER).unwrap().unwrap().covered,
        revision(0)
    );
    assert!(store.automation_receipts(CONSUMER, 20).unwrap().is_empty());
    let recovered = evaluate(&failed, &host, &trigger, true);
    assert_eq!(recovered.state.unwrap().covered, revision(2));
    assert_eq!(recovered.receipts.len(), 1);
}

/// A review task that closed with malformed coverage once held its consumer
/// `admitted` forever: the bytes could never change, so it never settled.
#[test]
fn a_stopped_action_with_malformed_evidence_settles_and_spends_its_retry_budget() {
    let (store, host, trigger) = setup();
    host.page(0, 2);
    let first = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    host.page(2, 2);
    // The incident's shape: an object where the schema expects a string.
    *host.raw_evidence.lock().unwrap() = Some(br#"{"schema_version":1,"batch_id":{}}"#.to_vec());

    // While the task is open its executor can still re-put the file.
    let open = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(open.state, BatchState::Admitted);
    assert!(open.reason.unwrap().contains("malformed"));

    host.stopped.store(true, Ordering::SeqCst);
    let settled = evaluate(store.as_ref(), &host, &trigger, true)
        .state
        .unwrap()
        .active
        .unwrap();
    assert_eq!(settled.state, BatchState::Claimed);
    assert_eq!(settled.attempt, 2);
    assert!(settled.action_id.is_none());
    assert!(
        settled
            .reason
            .as_deref()
            .unwrap()
            .contains("invalid type: map, expected a string"),
        "the parse error is recorded: {:?}",
        settled.reason
    );

    // After the backoff the next fire admits a fresh action over the same debt.
    let retried = evaluate_at(store.as_ref(), &host, &trigger, true, 6)
        .unwrap()
        .state
        .unwrap();
    let attempt = retried.active.unwrap();
    assert_eq!(attempt.state, BatchState::Admitted);
    assert_ne!(attempt.action_id, first.action_id);
    assert_eq!(attempt.batch, first.batch);
    assert_eq!(retried.covered, revision(0));

    // The retry closes the same way: the budget is spent, nothing is covered.
    let exhausted = evaluate_at(store.as_ref(), &host, &trigger, true, 7).unwrap();
    assert_eq!(exhausted.reason, "needs_attention");
    let state = exhausted.state.unwrap();
    assert_eq!(state.active.unwrap().state, BatchState::Exhausted);
    assert_eq!(state.covered, revision(0));
    assert!(exhausted.receipts.is_empty());
}
