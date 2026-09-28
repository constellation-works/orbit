use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::*;
use orbit_types::workflow::PipelineState;

use super::super::drain::{PullDrain, PullLauncher, PullPeer};
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;

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
}
impl PullPeer for Peer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.requests.set(self.requests.get() + 1);
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
                    phase: ExecutionClaimPhase::Claimed,
                }),
                task: (!self.idle.get()).then(|| AdmissionTaskSummary {
                    id: "task".into(),
                    title: "task".into(),
                    complexity: None,
                    crew: None,
                    context_files: vec!["file:src.rs".into()],
                }),
                invalid_candidates: vec![],
                deferred_conflicts: vec![],
                queue_depth: 0,
            })
            .clone();
        if self.lose_request.replace(false) {
            return Err(OrbitError::Execution("lost request response".into()));
        }
        Ok(receipt)
    }
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = &admission
            .receipt
            .as_ref()
            .expect("receipt")
            .claim
            .as_ref()
            .expect("claim")
            .claim_id;
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
        Ok(())
    }
    fn lookup(
        &self,
        _destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.lookups.set(self.lookups.get() + 1);
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
    fail: Cell<bool>,
}
impl PullLauncher for Launcher {
    fn launch(&self, record: &LocalPullAdmission) -> Result<(), OrbitError> {
        assert_eq!(record.phase, LocalPullPhase::Launching);
        self.launches.set(self.launches.get() + 1);
        if self.fail.get() {
            return Err(OrbitError::Execution("launch failed".into()));
        }
        Ok(())
    }
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
            caller_schema: 1,
            caller_review_policy: "none".into(),
            run_context: AdmissionRunContext {
                run_id: parent.run_id,
                job_name: "workspace_auto_pipeline".into(),
                machine_name: None,
            },
            ship: AdmissionShipContract {
                mode: "local".into(),
                base_branch: "main".into(),
                landing_branch: "main".into(),
                review_policy: "none".into(),
                completion: "review".into(),
                authorization_reference: None,
            },
        },
    )
}

#[test]
fn pull_lost_request_and_binding_responses_recover_the_same_leaf() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_lost_request_and_binding_responses_recover_the_same_leaf",
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

#[test]
fn pull_idle_ends_each_refill_after_one_request() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_idle_ends_each_refill_after_one_request",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    peer.idle.set(true);
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    assert_eq!(drain.refill(&destination, &template, 10).expect("idle"), 0);
    assert_eq!(peer.requests.get(), 1);
    assert_eq!(
        drain
            .refill(&destination, &template, 10)
            .expect("next poll"),
        0
    );
    assert_eq!(peer.requests.get(), 2);
    assert_eq!(launcher.launches.get(), 0);
}

#[test]
fn pull_launch_failure_persists_disconnected_settlement() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_launch_failure_persists_disconnected_settlement",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    peer.disconnected.set(true);
    let launcher = Launcher::default();
    launcher.fail.set(true);
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    assert!(drain.refill(&destination, &template, 1).is_err());
    let record = jobs.local_pull_admissions().expect("pending").remove(0);
    assert_eq!(record.phase, LocalPullPhase::Settling);
    assert!(matches!(record.settlement, Some(ClaimMutation::Fail(_))));
    assert_eq!(launcher.launches.get(), 1);
    peer.disconnected.set(false);
    // A zero-ceiling reconciliation performs no new admission.
    drain.refill(&destination, &template, 0).expect("settle");
    assert_eq!(
        jobs.local_pull_admissions().expect("records")[0].phase,
        LocalPullPhase::Settled
    );
    assert_eq!(peer.settlements.get(), 1);
    assert_eq!(launcher.launches.get(), 1);
}

#[test]
fn pull_stop_preserves_children_and_local_binding_refuses_generic_execution() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_stop_preserves_children_and_local_binding_refuses_generic_execution",
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
    };
    drain.refill(&destination, &template, 1).expect("admit");
    let leaf = jobs.local_pull_admissions().expect("record")[0]
        .leaf_run_id
        .clone()
        .expect("leaf");
    assert!(
        runtime
            .execute_pipeline_run_worker(&leaf)
            .expect_err("legacy path refused")
            .to_string()
            .contains("handoff execution adapter")
    );
    assert!(
        runtime
            .submit_resume_run(&leaf, None, None)
            .expect_err("resume refused")
            .to_string()
            .contains("deliberately recover")
    );
    jobs.finalize_job_run(
        &template.run_context.run_id,
        orbit_types::workflow::JobRunState::Cancelled,
        Utc::now(),
        None,
    )
    .expect("stop parent");
    drain
        .refill(&destination, &template, 10)
        .expect("no new admissions");
    assert_eq!(launcher.launches.get(), 1);
    assert_eq!(
        jobs.get_job_run(&leaf).expect("read").expect("child").state,
        orbit_types::workflow::JobRunState::Pending
    );
}

#[test]
fn pull_cancelled_queued_leaf_settles_without_launch_after_lost_bind() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_cancelled_queued_leaf_settles_without_launch_after_lost_bind",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    peer.lose_bind.set(true);
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    assert!(drain.refill(&destination, &template, 1).is_err());
    let record = jobs.local_pull_admissions().expect("admission").remove(0);
    let leaf = record.leaf_run_id.expect("leaf");
    jobs.finalize_job_run(
        &leaf,
        orbit_types::workflow::JobRunState::Cancelled,
        Utc::now(),
        None,
    )
    .expect("cancel queued leaf");
    peer.disconnected.set(true);
    assert!(drain.refill(&destination, &template, 0).is_err());
    assert_eq!(
        jobs.local_pull_admissions().expect("pending")[0].phase,
        LocalPullPhase::Settling
    );
    assert_eq!(launcher.launches.get(), 0);
    peer.disconnected.set(false);
    drain
        .refill(&destination, &template, 0)
        .expect("retry settlement");
    assert_eq!(
        jobs.local_pull_admissions().expect("settled")[0].phase,
        LocalPullPhase::Settled
    );
    assert_eq!(
        jobs.get_job_run(&leaf).expect("read").expect("leaf").state,
        orbit_types::workflow::JobRunState::Cancelled
    );
    assert_eq!(launcher.launches.get(), 0);
    assert_eq!(peer.settlements.get(), 1);
}

/// [ORB-12616] A follower's bound PR claim selects the handoff-only PR leaf.
///
/// The leaf a claim runs is chosen by the owner-resolved ship mode inside the
/// admission record, never by run input, and the definition it names is the
/// claimed one: the merge-capable `task_pr_pipeline` is not reachable from a
/// claim. The created run also carries no completion input, so there is no
/// value a later step could read as authority to land.
#[test]
fn pull_pr_mode_binds_the_claimed_pr_leaf_with_no_completion_authority() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_pr_mode_binds_the_claimed_pr_leaf_with_no_completion_authority",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (mut destination, mut template) = request(jobs);
    // A follower executes elsewhere; only PR mode is open to it.
    destination.execution_machine_id = "follower".into();
    template.ship.mode = "pr".into();
    let peer = Peer::default();
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    drain.refill(&destination, &template, 1).expect("admit");
    let leaf = jobs.local_pull_admissions().expect("record")[0]
        .leaf_run_id
        .clone()
        .expect("leaf");
    let run = jobs.get_job_run(&leaf).expect("read").expect("leaf run");
    assert_eq!(run.job_id, "task_claimed_pr_pipeline");
    assert!(
        jobs.list_job_runs("task_pr_pipeline")
            .expect("legacy leaves")
            .is_empty(),
        "a claim must never start the merge-capable legacy leaf"
    );
    let input = run.input.expect("leaf input");
    assert!(
        input.get("completion").is_none(),
        "a claimed leaf carries no completion authority: {input}"
    );
    assert_eq!(input["base_sync"], "remote");
}

fn isolated_pull_test(name: &str) -> bool {
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
        "child did not execute exact test: {stdout}"
    );
    true
}

/// [ORB-13625] An owner that answers a never-admitted request with a refusal
/// holds no receipt for it: the request closes as `Refused`, releases its
/// slot, and the pass reports the refusal instead of allocating more.
#[test]
fn pull_owner_refusal_without_a_receipt_closes_the_request_and_frees_its_slot() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_owner_refusal_without_a_receipt_closes_the_request_and_frees_its_slot",
    ) {
        return;
    }
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let peer = Peer::default();
    *peer.refuse.borrow_mut() = Some("version_mismatch".into());
    let launcher = Launcher::default();
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    let error = drain
        .refill(&destination, &template, 3)
        .expect_err("refusal reported");
    assert!(error.to_string().contains("version_mismatch"), "{error}");
    // One request went out; the refusal stopped the pass.
    assert_eq!(peer.requests.get(), 1);
    assert_eq!(peer.lookups.get(), 1);
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].phase, LocalPullPhase::Refused);
    assert!(
        records[0]
            .refusal
            .as_deref()
            .unwrap_or_default()
            .contains("version_mismatch")
    );
    assert_eq!(drain.unsettled(&destination).expect("unsettled"), 0);
    assert_eq!(jobs.drain_leaf_occupancy().expect("occupancy").occupied, 0);

    // A closed request is history: the next pass sends a new ID.
    *peer.refuse.borrow_mut() = None;
    assert_eq!(drain.refill(&destination, &template, 1).expect("admits"), 1);
    assert_eq!(peer.requests.get(), 2);
    assert_eq!(launcher.launches.get(), 1);
}

/// [ORB-13625] A retry can be refused although an earlier send of the same ID
/// committed — the owner was upgraded while the answer was lost. The owner's
/// receipt is the truth, so the claim is carried forward rather than
/// abandoned on the owner.
#[test]
fn pull_refused_retry_of_a_committed_request_carries_the_claim_forward() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_refused_retry_of_a_committed_request_carries_the_claim_forward",
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
    };
    peer.lose_request.set(true);
    assert!(drain.refill(&destination, &template, 1).is_err());
    assert_eq!(peer.receipts.borrow().len(), 1, "the owner committed");

    *peer.refuse.borrow_mut() = Some("version_mismatch".into());
    drain
        .refill(&destination, &template, 1)
        .expect("reconciled");
    assert_eq!(peer.lookups.get(), 1);
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].phase, LocalPullPhase::Launched);
    assert_eq!(launcher.launches.get(), 1);
}

/// [ORB-13625] A transport failure is not an owner answer: the request stays
/// pending for the same ID, and nothing is looked up or closed.
#[test]
fn pull_transport_failure_keeps_the_request_pending_for_the_same_id() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_transport_failure_keeps_the_request_pending_for_the_same_id",
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
    };
    peer.lose_request.set(true);
    assert!(drain.refill(&destination, &template, 1).is_err());
    assert_eq!(peer.lookups.get(), 0);
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records[0].phase, LocalPullPhase::Requested);
    assert_eq!(drain.unsettled(&destination).expect("unsettled"), 1);
    // Reconciling without allocating retries exactly that request.
    drain.reconcile_pending(&destination).expect("reconcile");
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].phase, LocalPullPhase::Launched);
}

#[test]
fn owner_refusals_are_distinguished_from_lost_or_uncertain_deliveries() {
    use super::super::drain::is_owner_refusal;
    for code in ["invalid_input", "capability_refused", "policy_denied"] {
        assert!(is_owner_refusal(&OrbitError::RemoteTool {
            code: code.into(),
            message: String::new(),
            payload: serde_json::Value::Null,
        }));
    }
    for error in [
        OrbitError::RemoteTool {
            code: "store_error".into(),
            message: String::new(),
            payload: serde_json::Value::Null,
        },
        OrbitError::OutcomeUnknown {
            mcp_call_id: "1".into(),
            message: String::new(),
        },
        OrbitError::UnreachableDestination("owner".into()),
        OrbitError::Execution("lost".into()),
    ] {
        assert!(!is_owner_refusal(&error), "{error}");
    }
}

/// [ORB-13625] Fast-failing leaves free their slots within seconds; the
/// breaker counts this drain's consecutive failed settlements so a systemic
/// executor fault stops claiming the owner's backlog.
#[test]
fn pull_breaker_counts_this_drains_consecutive_failed_settlements() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_breaker_counts_this_drains_consecutive_failed_settlements",
    ) {
        return;
    }
    use super::super::refill::{CONSECUTIVE_FAILURE_BREAKER, consecutive_failed_settlements};
    let (_temp, runtime, _repo) = runtime_with_workspace_layout();
    let jobs = runtime.stores().jobs();
    let (destination, template) = request(jobs);
    let run_id = template.run_context.run_id.clone();
    let peer = Peer::default();
    let launcher = Launcher::default();
    launcher.fail.set(true);
    let drain = PullDrain {
        jobs,
        peer: &peer,
        launcher: &launcher,
    };
    for expected in 1..=CONSECUTIVE_FAILURE_BREAKER {
        assert!(drain.refill(&destination, &template, 1).is_err());
        assert_eq!(
            consecutive_failed_settlements(&runtime, &destination, &run_id).expect("count"),
            expected
        );
    }
    // Another drain's history never trips this one.
    assert_eq!(
        consecutive_failed_settlements(&runtime, &destination, "another-drain").expect("count"),
        0
    );
}

/// Admit `count` claims, then end every leaf, so the next pass settles each.
fn admitted_ended_leaves(
    jobs: &dyn JobRunStoreBackend,
    drain: &PullDrain<'_>,
    destination: &PullDestination,
    template: &AdmissionRequest,
    count: usize,
) -> Vec<LocalPullAdmission> {
    assert_eq!(
        drain.refill(destination, template, count).expect("admit"),
        count
    );
    let records = jobs.local_pull_admissions().expect("records");
    for record in &records {
        jobs.finalize_job_run(
            record.leaf_run_id.as_deref().expect("leaf"),
            orbit_types::workflow::JobRunState::Cancelled,
            Utc::now(),
            None,
        )
        .expect("leaf ended");
    }
    records
}

fn claim_id(record: &LocalPullAdmission) -> String {
    record
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.claim.as_ref())
        .expect("claim")
        .claim_id
        .clone()
}

/// [ORB-13639] A settlement for a claim the owner already ended — an operator revoked it
/// while the drain that ran it was down — can never be accepted. The owner's
/// receipt confirms the claim is over, so the record settles locally with the
/// refusal, frees its slot, and the same pass admits new work instead of
/// retrying that settlement forever.
#[test]
fn pull_settlement_for_a_claim_the_owner_ended_closes_locally_and_frees_its_slot() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_settlement_for_a_claim_the_owner_ended_closes_locally_and_frees_its_slot",
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
    };
    let revoked = admitted_ended_leaves(jobs, &drain, &destination, &template, 1).remove(0);
    peer.ended
        .borrow_mut()
        .insert(claim_id(&revoked), ExecutionClaimPhase::Revoked);

    assert_eq!(
        drain.refill(&destination, &template, 1).expect("admits"),
        1,
        "the closed settlement no longer holds the only slot"
    );
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].phase, LocalPullPhase::Settled);
    assert!(matches!(
        records[0].settlement,
        Some(ClaimMutation::Fail(_))
    ));
    let refusal = records[0].refusal.as_deref().unwrap_or_default();
    assert!(
        refusal.contains("stale_claim") && refusal.contains("Revoked"),
        "{refusal}"
    );
    assert_eq!(peer.settlements.get(), 0, "nothing reached the owner");
    assert_eq!(launcher.launches.get(), 2);
}

/// A refused settlement for a claim the owner still holds is not obsolete:
/// it stays pending for the next pass, and the pass reports the refusal.
#[test]
fn pull_refused_settlement_for_a_claim_the_owner_still_holds_stays_pending() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_refused_settlement_for_a_claim_the_owner_still_holds_stays_pending",
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
    };
    let live = admitted_ended_leaves(jobs, &drain, &destination, &template, 1).remove(0);
    peer.ended
        .borrow_mut()
        .insert(claim_id(&live), ExecutionClaimPhase::Running);

    let error = drain
        .refill(&destination, &template, 1)
        .expect_err("refusal reported");
    assert!(error.to_string().contains("stale_claim"), "{error}");
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records.len(), 1, "a pass that failed admits nothing new");
    assert_eq!(records[0].phase, LocalPullPhase::Settling);
    assert_eq!(records[0].refusal, None);
    assert_eq!(drain.unsettled(&destination).expect("unsettled"), 1);
}

/// One settlement that cannot be delivered does not hold back the others:
/// each record is carried forward, and the error is reported afterwards.
#[test]
fn pull_one_stuck_settlement_does_not_hold_back_the_others() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::pull::tests::drain::pull_one_stuck_settlement_does_not_hold_back_the_others",
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
    };
    let records = admitted_ended_leaves(jobs, &drain, &destination, &template, 2);
    peer.ended
        .borrow_mut()
        .insert(claim_id(&records[0]), ExecutionClaimPhase::Running);

    assert!(drain.reconcile_pending(&destination).is_err());
    let records = jobs.local_pull_admissions().expect("records");
    assert_eq!(records[0].phase, LocalPullPhase::Settling);
    assert_eq!(records[1].phase, LocalPullPhase::Settled);
    assert_eq!(peer.settlements.get(), 1);
}

/// The owner cannot read a follower's run, and the failure settlement becomes
/// the blocked task's `execution_summary`, so it quotes the failed step.
#[test]
fn a_terminal_failure_settlement_names_the_failed_step_and_its_error() {
    let mut run: orbit_types::workflow::JobRun = serde_json::from_value(serde_json::json!({
        "run_id": "jrun-leaf",
        "job_id": "task_claimed_pr_pipeline",
        "attempt": 1,
        "state": "failed",
        "scheduled_at": Utc::now(),
        "created_at": Utc::now(),
    }))
    .expect("leaf run");
    run.steps.push(failed_step(
        "agent_implement",
        "cli subprocess reported declared envelope status=\"failed\"",
    ));
    let summary = super::super::drain::terminal_failure_summary(&run);
    assert!(summary.starts_with("Outcome: failed"), "{summary}");
    assert!(summary.contains("jrun-leaf"), "{summary}");
    assert!(
        summary.contains("Failed step: agent_implement (STEP_FAILED)"),
        "{summary}"
    );
    assert!(summary.contains("declared envelope status"), "{summary}");

    run.steps.clear();
    run.steps
        .push(failed_step("agent_implement", &"e".repeat(20_000)));
    let bounded = super::super::drain::terminal_failure_summary(&run);
    assert!(
        bounded.contains("[truncated to"),
        "a runaway error is bounded"
    );
    assert!(bounded.len() < 10_000);
}

fn failed_step(target: &str, message: &str) -> orbit_types::workflow::JobRunStep {
    orbit_types::workflow::JobRunStep {
        step_index: 0,
        target_type: orbit_types::workflow::JobTargetType::Activity,
        target_id: target.into(),
        started_at: None,
        finished_at: None,
        duration_ms: None,
        exit_code: Some(1),
        agent_response_json: None,
        state: orbit_types::workflow::JobRunState::Failed,
        error_code: Some("STEP_FAILED".into()),
        error_message: Some(message.into()),
    }
}
