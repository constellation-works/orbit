use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::*;
use orbit_types::workflow::PipelineState;

use super::super::pull::{PullDrain, PullLauncher, PullPeer};
use super::super::test_support::runtime_with_workspace_layout;

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
}
impl PullPeer for Peer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.requests.set(self.requests.get() + 1);
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
                        host_id: None,
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
        self.settlements.set(self.settlements.get() + 1);
        Ok(())
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
                host_id: None,
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
        "adapter::engine_host::v2_host::tests::pull::pull_lost_request_and_binding_responses_recover_the_same_leaf",
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
        jobs.list_job_runs("task_local_pipeline")
            .expect("leaves")
            .len(),
        1
    );
}

#[test]
fn pull_idle_ends_each_refill_after_one_request() {
    if isolated_pull_test(
        "adapter::engine_host::v2_host::tests::pull::pull_idle_ends_each_refill_after_one_request",
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
        "adapter::engine_host::v2_host::tests::pull::pull_launch_failure_persists_disconnected_settlement",
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
        "adapter::engine_host::v2_host::tests::pull::pull_stop_preserves_children_and_local_binding_refuses_generic_execution",
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
        "adapter::engine_host::v2_host::tests::pull::pull_cancelled_queued_leaf_settles_without_launch_after_lost_bind",
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
