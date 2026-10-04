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
    /// Every call the owner was sent, whatever it answered.
    owner_calls: Cell<usize>,
    /// Answer every call as an unreachable destination.
    unreachable: Cell<bool>,
    /// Let this many requests through, then answer the rest as unreachable.
    fail_requests_after: Cell<Option<usize>>,
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
    fn cancel_queued(&self, _record: &LocalPullAdmission) -> Result<(), OrbitError> {
        Err(OrbitError::Execution(
            "a live drain never cancels its queued leaves".into(),
        ))
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
