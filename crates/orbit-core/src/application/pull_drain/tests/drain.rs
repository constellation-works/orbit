use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::*;
use orbit_types::workflow::{
    PipelineState, ReviewBudget, ReviewConsumption, ReviewLedger, ReviewResetDecision,
};

use super::super::drain::{PullDrain, PullLauncher, PullPeer, RefusedDelivery, no_claimed_review};
use crate::application::job::pipeline::WorkerLaunchError;
use crate::application::test_support::runtime_with_workspace_layout;

#[derive(Default)]
struct Peer {
    receipts: RefCell<BTreeMap<String, AdmissionReceipt>>,
    requests: Cell<usize>,
    binds: RefCell<BTreeMap<String, String>>,
    lose_request: Cell<bool>,
    lose_bind: Cell<bool>,
    disconnected: Cell<bool>,
    settlements: Cell<usize>,
    idle: Cell<bool>,
    /// Answer every request with this owner refusal, whether or not a
    /// receipt already exists — the way an upgraded owner refuses a replay.
    refuse: RefCell<Option<String>>,
    lookups: Cell<usize>,
    /// Claims whose settlement the owner refuses as `stale_claim`, with the
    /// phase its receipt lookup then reports for each.
    ended: RefCell<BTreeMap<String, ExecutionClaimPhase>>,
    /// Every call the owner was sent, whatever it answered.
    owner_calls: Cell<usize>,
    /// Answer every call as an unreachable destination.
    unreachable: Cell<bool>,
    /// Let this many requests through, then answer the rest as unreachable.
    fail_requests_after: Cell<Option<usize>>,
    /// The candidate the owner kept from the task's last claim, offered with
    /// every claim.
    resume_candidate: RefCell<Option<ClaimCandidateRef>>,
    /// Every settlement delivered, in order.
    settled: RefCell<Vec<ClaimMutation>>,
}
impl Peer {
    /// Count a call and answer it as the transport would when the owner is
    /// down.
    fn reach(&self) -> Result<(), OrbitError> {
        self.owner_calls.set(self.owner_calls.get() + 1);
        if self.unreachable.get() {
            return Err(OrbitError::UnreachableDestination(
                "owner: ssh: connect timed out".into(),
            ));
        }
        Ok(())
    }
}
impl PullPeer for Peer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.requests.set(self.requests.get() + 1);
        self.reach()?;
        if self
            .fail_requests_after
            .get()
            .is_some_and(|allowed| self.requests.get() > allowed)
        {
            return Err(OrbitError::UnreachableDestination(
                "owner: ssh: connect timed out".into(),
            ));
        }
        if let Some(message) = self.refuse.borrow().clone() {
            return Err(OrbitError::RemoteTool {
                code: "invalid_input".into(),
                message,
                payload: serde_json::Value::Null,
            });
        }
        let receipt = self
            .receipts
            .borrow_mut()
            .entry(request.request_id.clone())
            .or_insert_with(|| AdmissionReceipt {
                schema_version: 1,
                gates: None,
                tag_instructions: None,
                affinity: None,
                request: request.clone(),
                machine_id: destination.execution_machine_id.clone(),
                claim: (!self.idle.get()).then(|| ExecutionClaim {
                    claim_id: format!("claim-{}", request.request_id),
                    task_id: "task".into(),
                    request_id: request.request_id.clone(),
                    executed_on: ExecutionLocation {
                        machine_id: destination.execution_machine_id.clone(),
                        machine_name: None,
                    },
                    run_context: request.run_context.clone(),
                    footprint: vec!["file:src.rs".into()],
                    reservation_id: "reservation".into(),
                    reservation_expires_at: "later".into(),
                    repair: None,
                    phase: ExecutionClaimPhase::Claimed,
                }),
                task: (!self.idle.get()).then(|| AdmissionTaskSummary {
                    id: "task".into(),
                    title: "task".into(),
                    complexity: None,
                    crew: None,
                    crew_source: None,
                    context_files: vec!["file:src.rs".into()],
                    resume_candidate: self.resume_candidate.borrow().clone(),
                }),
                invalid_candidates: vec![],
                deferred_conflicts: vec![],
                crew_unavailable: vec![],
                os_unavailable: vec![],
                browser_kit_unavailable: vec![],
                queue_depth: 0,
                capacity_unavailable: Vec::new(),
            })
            .clone();
        if self.lose_request.replace(false) {
            return Err(OrbitError::Execution("lost request response".into()));
        }
        Ok(receipt)
    }
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.reach()?;
        let claim = &admission
            .receipt
            .as_ref()
            .expect("receipt")
            .claim
            .as_ref()
            .expect("claim")
            .claim_id;
        // A claim the owner has already ended refuses a bind the way it
        // refuses a settlement, and records nothing.
        if self.ended.borrow().contains_key(claim) {
            return Err(OrbitError::RemoteTool {
                code: "invalid_input".into(),
                message: "owner: invalid input: stale_claim".into(),
                payload: serde_json::Value::Null,
            });
        }
        let run = admission.leaf_run_id.as_ref().expect("leaf");
        let mut binds = self.binds.borrow_mut();
        assert_eq!(
            binds.entry(claim.clone()).or_insert_with(|| run.clone()),
            run
        );
        if self.lose_bind.replace(false) {
            return Err(OrbitError::Execution("lost bind response".into()));
        }
        Ok(())
    }
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        assert!(admission.settlement.is_some());
        self.reach()?;
        if self.disconnected.get() {
            return Err(OrbitError::Execution("disconnected".into()));
        }
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref())
            .expect("claim");
        if self.ended.borrow().contains_key(&claim.claim_id) {
            return Err(OrbitError::RemoteTool {
                code: "invalid_input".into(),
                message: "owner: invalid input: stale_claim".into(),
                payload: serde_json::Value::Null,
            });
        }
        self.settlements.set(self.settlements.get() + 1);
        self.settled
            .borrow_mut()
            .extend(admission.settlement.clone());
        Ok(())
    }
    fn lookup(
        &self,
        _destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.lookups.set(self.lookups.get() + 1);
        self.reach()?;
        Ok(match self.receipts.borrow().get(request_id) {
            Some(receipt) => AdmissionLookup::Found {
                receipt: Box::new(receipt.clone()),
                current_claim: receipt.claim.as_ref().and_then(|claim| {
                    let phase = *self.ended.borrow().get(&claim.claim_id)?;
                    Some(Box::new(ExecutionClaim {
                        phase,
                        ..claim.clone()
                    }))
                }),
            },
            None => AdmissionLookup::NotFound,
        })
    }
}
#[derive(Default)]
struct Launcher {
    launches: Cell<usize>,
}
impl PullLauncher for Launcher {
    fn launch(&self, record: &LocalPullAdmission) -> Result<(), WorkerLaunchError> {
        assert_eq!(record.phase, LocalPullPhase::Launching);
        self.launches.set(self.launches.get() + 1);
        Ok(())
    }
    fn cancel_queued(&self, _record: &LocalPullAdmission) -> Result<(), OrbitError> {
        Err(OrbitError::Execution(
            "a live drain never cancels its queued leaves".into(),
        ))
    }
}

/// Fault injection: a spawned child can lose its observer before acknowledging
/// launch. A pending run alone does not prove that no worker executed.
#[test]
fn a_post_spawn_error_keeps_launch_intent_and_never_releases_pending_work() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::a_post_spawn_error_keeps_launch_intent_and_never_releases_pending_work",
    ) {
        return;
    }
    struct UncertainLauncher;
    impl PullLauncher for UncertainLauncher {
        fn launch(&self, _: &LocalPullAdmission) -> Result<(), WorkerLaunchError> {
            Err(WorkerLaunchError::Uncertain(OrbitError::Execution(
                "observer closed after spawn".into(),
            )))
        }
        fn cancel_queued(&self, _: &LocalPullAdmission) -> Result<(), OrbitError> {
            panic!("post-spawn uncertainty must not cancel as never launched");
        }
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &UncertainLauncher,
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &no_claimed_review,
    };
    assert!(drain.refill(&destination, &template, 1).is_err());
    let [record] = jobs.local_pull_admissions().unwrap().try_into().unwrap();
    let record: LocalPullAdmission = record;
    assert_eq!(record.phase, LocalPullPhase::Launching);
    assert!(record.settlement.is_none());
    assert!(drain.reconcile_pending(&destination).is_err());
    assert!(
        peer.settled.borrow().is_empty(),
        "an uncertain launch keeps the owner's claim"
    );
    assert_eq!(peer.requests.get(), 1, "uncertainty admits no replacement");
}

/// Fault injection: another process records the settlement during launch, and
/// the first attempt to cancel the queued leaf fails. Reconciliation adopts
/// the first settlement and retries cancellation before giving back the task.
#[test]
fn a_launch_failure_adopts_a_concurrent_settlement_and_retries_queued_cancellation() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::a_launch_failure_adopts_a_concurrent_settlement_and_retries_queued_cancellation",
    ) {
        return;
    }
    struct InterleavedLauncher<'a> {
        jobs: &'a dyn JobRunStoreBackend,
        cancellations: Cell<usize>,
        settlement: RefCell<Option<ClaimMutation>>,
    }
    impl PullLauncher for InterleavedLauncher<'_> {
        fn launch(&self, record: &LocalPullAdmission) -> Result<(), WorkerLaunchError> {
            let settlement = super::super::drain::release_settlement(record, "concurrent release");
            self.jobs.mutate_local_pull(
                &record.destination,
                &record.request.request_id,
                &LocalPullMutation::Settle(Box::new(settlement.clone())),
            )?;
            *self.settlement.borrow_mut() = Some(settlement);
            Err(OrbitError::Execution("injected pre-spawn failure".into()).into())
        }
        fn cancel_queued(&self, record: &LocalPullAdmission) -> Result<(), OrbitError> {
            let attempt = self.cancellations.get() + 1;
            self.cancellations.set(attempt);
            if attempt == 1 {
                return Err(OrbitError::Execution(
                    "injected cancellation failure".into(),
                ));
            }
            self.jobs.finalize_job_run(
                record.leaf_run_id.as_deref().unwrap(),
                orbit_types::workflow::JobRunState::Cancelled,
                Utc::now(),
                None,
            )?;
            Ok(())
        }
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    let launcher = InterleavedLauncher {
        jobs,
        cancellations: Cell::new(0),
        settlement: RefCell::new(None),
    };
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &no_claimed_review,
    };
    let error = drain.refill(&destination, &template, 1).unwrap_err();
    assert!(
        error.to_string().contains("injected cancellation failure"),
        "the concurrent settlement is adopted instead of an immutable-settlement error: {error}"
    );
    assert!(
        peer.settled.borrow().is_empty(),
        "cancellation must succeed before delivery"
    );
    drain.reconcile_pending(&destination).unwrap();
    assert_eq!(launcher.cancellations.get(), 2);
    let settlements = peer.settled.borrow();
    assert_eq!(
        settlements.as_slice(),
        &[launcher.settlement.borrow().clone().unwrap()],
        "the first recorded settlement is delivered unchanged"
    );
    let records = jobs.local_pull_admissions().unwrap();
    assert_eq!(records[0].phase, LocalPullPhase::Settled);
    assert_eq!(
        jobs.get_job_run(records[0].leaf_run_id.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .state,
        orbit_types::workflow::JobRunState::Cancelled
    );
}
fn request(jobs: &dyn JobRunStoreBackend) -> (PullDestination, AdmissionRequest) {
    let parent = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("parent");
    jobs.write_run_state(
        &parent.run_id,
        &PipelineState::new(parent.run_id.clone(), parent.job_id, serde_json::json!({})),
    )
    .expect("state");
    (
        PullDestination {
            owner_machine_id: "owner".into(),
            owner_workspace_id: "ws".into(),
            selector: "owner/ws".into(),
            execution_machine_id: "owner".into(),
        },
        AdmissionRequest {
            request_id: "template".into(),
            caller_version: "1".into(),
            caller_schema: orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            caller_fingerprint: None,
            caller_before_pr: false,
            review_gate: false,
            run_context: AdmissionRunContext {
                run_id: parent.run_id,
                job_name: "workspace_auto_pipeline".into(),
                machine_name: None,
            },
            ship: AdmissionShipContract {
                mode: "local".into(),
                base_branch: "main".into(),
                landing_branch: "main".into(),
                before_pr: false,
                before_landing: false,
                completion: "review".into(),
                authorization_reference: None,
                review: None,
            },
            crews: None,
            os: None,
            browser_kit: false,
            admissible: None,
        },
    )
}

#[test]
fn pull_lost_request_and_binding_responses_recover_the_same_leaf() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::pull_lost_request_and_binding_responses_recover_the_same_leaf",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &no_claimed_review,
    };
    peer.lose_request.set(true);
    assert!(drain.refill(&destination, &template, 1).is_err());
    peer.lose_bind.set(true);
    assert!(drain.refill(&destination, &template, 1).is_err());
    assert_eq!(launcher.launches.get(), 0);
    drain.refill(&destination, &template, 1).expect("recover");
    drain.refill(&destination, &template, 1).expect("saturated");
    assert_eq!(peer.receipts.borrow().len(), 1);
    assert_eq!(peer.binds.borrow().len(), 1);
    assert_eq!(launcher.launches.get(), 1);
    assert_eq!(
        jobs.list_job_runs("task_claimed_local_pipeline")
            .expect("leaves")
            .len(),
        1
    );
}

/// [ORB-14174] A stop or cancel recorded while a pass is requesting ends it
/// before the next request: the guard is asked before every allocation, and
/// the claims already made are kept.
#[test]
fn pull_admission_stop_mid_pass_ends_it_before_the_next_request() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::pull_admission_stop_mid_pass_ends_it_before_the_next_request",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &no_claimed_review,
    };
    let asked = Cell::new(0);
    // Open for the first request; the stop lands while it is answered.
    let admitting = || {
        asked.set(asked.get() + 1);
        Ok(asked.get() == 1)
    };

    let pass = drain.refill_pass(&destination, &|| Ok(Some(template.clone())), &admitting, 3);

    assert!(pass.error.is_none());
    assert_eq!(pass.admitted, 1);
    assert_eq!(peer.requests.get(), 1, "no request after the stop");
    assert_eq!(asked.get(), 2);
    assert_eq!(
        launcher.launches.get(),
        1,
        "the admitted claim still launches"
    );

    let failing = || Err(OrbitError::Store("run state unreadable".into()));
    let pass = drain.refill_pass(&destination, &|| Ok(Some(template.clone())), &failing, 3);
    assert!(pass.error.is_some(), "an unreadable stop ends the pass");
    assert_eq!(peer.requests.get(), 1);
}

/// [ORB-14338] A claimed owner-local leaf is handed the candidate its claim
/// carries, and when it fails after its commit, its settlement keeps the
/// committed worktree branch at the commit checkpoint for the next claim.
#[test]
fn pull_claimed_local_leaf_resumes_and_keeps_its_committed_candidate() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::pull_claimed_local_leaf_resumes_and_keeps_its_committed_candidate",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let kept = ClaimCandidateRef {
        branch: "orbit/task-earlier".into(),
        head_sha: "a".repeat(40),
        pull_request: None,
        pull_request_head: None,
        source_run_id: Some("jrun-earlier".into()),
        failed_step_id: Some("validate".into()),
        published: false,
        durable_ref: None,
        carry_failure: None,
        interrupted_review: None,
        landing_stop: None,
    };
    let peer = Peer {
        resume_candidate: RefCell::new(Some(kept.clone())),
        ..Peer::default()
    };
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
        refused_delivery: RefusedDelivery::WhenDue,
        claimed_review: &no_claimed_review,
    };
    drain.refill(&destination, &template, 1).expect("admit");
    let [leaf] = jobs
        .list_job_runs("task_claimed_local_pipeline")
        .expect("leaves")
        .try_into()
        .expect("one claimed-local leaf");
    let input = leaf.input.clone().expect("leaf input");
    assert_eq!(
        serde_json::from_value::<ClaimCandidateRef>(input["resume_candidate"].clone())
            .expect("the leaf input carries the candidate"),
        kept
    );

    let committed = "b".repeat(40);
    let mut state = jobs
        .read_run_state(&leaf.run_id)
        .expect("state")
        .unwrap_or_else(|| {
            PipelineState::new(leaf.run_id.clone(), leaf.job_id.clone(), input.clone())
        });
    state.pipeline["worktree"] = serde_json::json!({"head_ref": "orbit/task-local"});
    state.pipeline["commit"] = serde_json::json!({"commit_sha": committed});
    jobs.write_run_state(&leaf.run_id, &state).expect("state");
    let now = Utc::now();
    jobs.mark_job_run_running(&leaf.run_id, now, std::process::id())
        .expect("leaf running");
    jobs.complete_job_run_step(
        &leaf.run_id,
        &JobRunStepParams {
            lock_wait_ms: None,
            step_index: 3,
            target_type: orbit_types::workflow::JobTargetType::Activity,
            target_id: "validate".into(),
            started_at: now,
            finished_at: now,
            duration_ms: None,
            exit_code: Some(1),
            agent_response_json: None,
            state: orbit_types::workflow::JobRunState::Failed,
            error_code: None,
            error_message: Some(
                "[validation_environment] required validation could not run: cargo is missing"
                    .into(),
            ),
        },
    )
    .expect("failed step");
    jobs.finalize_job_run(
        &leaf.run_id,
        orbit_types::workflow::JobRunState::Failed,
        now,
        None,
    )
    .expect("leaf failed");
    drain.reconcile_pending(&destination).expect("settle");

    let settled = peer.settled.borrow();
    let [ClaimMutation::Release(evidence)] = settled.as_slice() else {
        panic!("one release: {settled:?}");
    };
    let candidate = evidence
        .failure
        .as_ref()
        .and_then(|failure| failure.candidate.as_ref())
        .expect("the release keeps the committed candidate");
    assert_eq!(candidate.branch, "orbit/task-local");
    assert_eq!(candidate.head_sha, committed);
    assert_eq!(candidate.failed_step_id.as_deref(), Some("validate"));
    assert_eq!(
        candidate.source_run_id.as_deref(),
        Some(leaf.run_id.as_str())
    );
}

/// [ORB-16699] The first settlement a claimed leaf records carries the
/// exhausted review ledger the follower holds, so the owner can import it.
#[test]
fn claimed_leaf_settlement_carries_its_review_ledger() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::claimed_leaf_settlement_carries_its_review_ledger",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let (claim_id, run_id) = {
        let jobs = runtime.stores().jobs();
        let (destination, template) = request(jobs);
        let peer = Peer::default();
        let launcher = Launcher::default();
        let drain = PullDrain {
            jobs,
            peer: &peer,
            launcher: &launcher,
            refused_delivery: RefusedDelivery::WhenDue,
            claimed_review: &no_claimed_review,
        };
        drain.refill(&destination, &template, 1).expect("admit");
        let records = jobs.local_pull_admissions().expect("pulls");
        let record = records.first().expect("one pull");
        let claim_id = record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref())
            .expect("the pull claimed a task")
            .claim_id
            .clone();
        let run_id = record.leaf_run_id.clone().expect("launched leaf");
        (claim_id, run_id)
    };
    let workspace = runtime.workspace_id().expect("workspace");
    let mut ledger = ReviewLedger::new(
        &format!("{workspace}/task/main/{claim_id}"),
        vec!["task".into()],
        ReviewBudget { minutes: 30 },
        Utc::now(),
    );
    ledger.consumed_seconds = 1_800;
    runtime
        .review_store()
        .expect("review store")
        .review_import_claimed_ledger(&workspace, &ledger)
        .expect("seed ledger");
    let now = Utc::now();
    {
        let jobs = runtime.stores().jobs();
        jobs.mark_job_run_running(&run_id, now, std::process::id())
            .expect("leaf running");
        jobs.complete_job_run_step(
            &run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: 1,
                target_type: orbit_types::workflow::JobTargetType::Activity,
                target_id: "implement".into(),
                started_at: now,
                finished_at: now,
                duration_ms: None,
                exit_code: Some(1),
                agent_response_json: None,
                state: orbit_types::workflow::JobRunState::Failed,
                error_code: None,
                error_message: Some("implementation failed".into()),
            },
        )
        .expect("failed step");
        jobs.finalize_job_run(
            &run_id,
            orbit_types::workflow::JobRunState::Failed,
            now,
            None,
        )
        .expect("leaf failed");
    }
    let settled = runtime
        .record_claimed_leaf_settlement(&run_id, None)
        .expect("record")
        .expect("the leaf had a pull");
    let ClaimMutation::Fail(evidence) = settled.settlement.as_ref().expect("a failure settlement")
    else {
        panic!("an untyped candidate failure fails the claim: {settled:?}");
    };
    let carry = evidence
        .claimed_review
        .as_ref()
        .expect("the settlement carries the follower ledger");
    assert_eq!(carry.ledgers.len(), 1, "{carry:?}");
    assert!(
        carry.ledgers[0]
            .lineage_key
            .ends_with(&format!("/{claim_id}")),
        "{}",
        carry.ledgers[0].lineage_key
    );
    assert_eq!(carry.ledgers[0].consumed_seconds, 1_800);
    assert!(carry.ledgers[0].decisions.is_empty());
}

/// [ORB-16699] Re-importing the exhausted ledger must not wipe an operator
/// reset that already recorded a decision.
#[test]
fn claimed_review_import_keeps_an_operator_decision() {
    if isolated_pull_test(
        "application::pull_drain::tests::drain::claimed_review_import_keeps_an_operator_decision",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let workspace = runtime.workspace_id().expect("workspace");
    let now = Utc::now();
    let mut decided = ReviewLedger::new(
        &format!("{workspace}/task/main/claim-kept"),
        vec!["task".into()],
        ReviewBudget { minutes: 30 },
        now,
    );
    decided.decisions.push(ReviewResetDecision {
        after_attempt_index: 0,
        reason: "operator reset".into(),
        actor: "operator".into(),
        recorded_at: now,
        previous_budget: ReviewBudget { minutes: 30 },
        previous_consumption: ReviewConsumption { seconds: 1_800 },
        budget: ReviewBudget { minutes: 30 },
    });
    let store = runtime.review_store().expect("review store");
    store
        .review_import_claimed_ledger(&workspace, &decided)
        .expect("seed decision");
    let mut exhausted = decided.clone();
    exhausted.decisions.clear();
    exhausted.consumed_seconds = 1_800;
    let kept = store
        .review_import_claimed_ledger(&workspace, &exhausted)
        .expect("replay");
    assert_eq!(kept.decisions.len(), 1);
    assert_eq!(kept.consumed_seconds, 0);
    assert_eq!(kept.decisions[0].reason, "operator reset");
}

pub(super) fn isolated_pull_test(name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_LOCAL_PULL_CHILD";
    if std::env::var(CHILD).ok().as_deref() == Some(name) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .output()
        .expect("isolated pull child");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{name}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "child did not execute exact test `{name}`: {stdout}"
    );
    true
}
