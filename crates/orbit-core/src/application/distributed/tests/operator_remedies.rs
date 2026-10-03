//! The refusals below name what the operator does next. They assert the
//! remedy (the flag, the capability, the config file), not the sentence.

use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_tools::{DrainOwnerTransport, OwnerCoordinator};

use super::*;
use crate::application::distributed::{DrainEntryPoint, WorkspacePullRequest};

/// A transport that records every delivery and answers it with `outcome`.
struct RecordingTransport {
    calls: Mutex<Vec<String>>,
    unknown_selector: bool,
}

impl DrainOwnerTransport for RecordingTransport {
    fn call(&self, selector: &str, name: &str, _: Value) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("{selector} {name}"));
        if self.unknown_selector {
            return Err(OrbitError::UnknownSelector(selector.to_string()));
        }
        Err(OrbitError::UnreachableDestination(selector.to_string()))
    }

    fn worker_coordinator(&self) -> Arc<dyn OwnerCoordinator> {
        unreachable!("a refused pull never binds a worker")
    }
}

fn replica_of_owner(transport: Arc<RecordingTransport>) -> OrbitRuntime {
    let (root, runtime, repo_root) = test_runtime();
    // The root guard must outlive the runtime for the whole test body.
    std::mem::forget(root);
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let runtime = OrbitRuntime::from_roots_with_binding(
        &runtime.global_root(),
        &repo_root.join(".orbit"),
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_replica".to_string(),
            task_partition_id: workspace_id,
            owner_machine_id: Some(OWNER.to_string()),
            repo_root: repo_root.clone(),
            ship_mode: ShipMode::Pr,
            base_branch: None,
        },
    )
    .expect("replica runtime");
    runtime
        .with_coordination_write_owner(Some(OWNER.to_string()))
        .with_automation_machine_identity(Some(FOLLOWER.to_string()))
        .with_drain_owner_transport(transport)
}

fn own_selector(runtime: &OrbitRuntime) -> String {
    let workspace = runtime
        .workspace_runtime_binding()
        .expect("test runtime is bound to a workspace")
        .logical_workspace_id
        .clone();
    format!("{OWNER}/{workspace}")
}

#[test]
fn a_replica_refused_at_an_entry_point_is_pointed_at_pull() {
    if !enter_isolated_child(
        "operator_remedies::a_replica_refused_at_an_entry_point_is_pointed_at_pull",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let replica = runtime.with_coordination_write_owner(Some(OWNER.to_string()));
    let error = replica
        .drain_entry_admission(DrainEntryPoint::OwnerDrain, &[], false)
        .expect("replica decision")
        .into_result()
        .expect_err("a replica cannot run the owner drain");
    assert!(matches!(error, OrbitError::CapabilityRefused(_)), "{error}");
    assert!(
        error.to_string().contains("--pull"),
        "the refusal must name the replica's own command: {error}"
    );
}

#[test]
fn a_replica_asked_an_owner_question_names_the_owner_and_how_to_reach_it() {
    if !enter_isolated_child(
        "operator_remedies::a_replica_asked_an_owner_question_names_the_owner_and_how_to_reach_it",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let replica = runtime.with_coordination_write_owner(Some(OWNER.to_string()));
    for tool in ["orbit.drain.probe", "orbit.drain.claims"] {
        let error = run_as(&replica, operator_session(), tool, json!({}))
            .expect_err("a replica serves no owner question");
        assert!(
            matches!(error, OrbitError::CapabilityRefused(_)),
            "{tool}: {error}"
        );
        let message = error.to_string();
        assert!(
            message.contains(OWNER),
            "{tool} must name the owner: {message}"
        );
        assert!(
            message.contains("selector"),
            "{tool} must say to reach the owner through its selector: {message}"
        );
    }
}

#[test]
fn cross_attempt_receipt_lookup_by_an_agent_names_the_operator_remedy() {
    if !enter_isolated_child(
        "operator_remedies::cross_attempt_receipt_lookup_by_an_agent_names_the_operator_remedy",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = test_runtime();
    let error = run_as(
        &runtime,
        follower_session(),
        "orbit.drain.receipt.lookup",
        json!({"request_id": "req-1", "machine_id": "hm_someone_else"}),
    )
    .expect_err("an agent reads only its own namespace");
    assert!(matches!(error, OrbitError::CapabilityRefused(_)), "{error}");
    let message = error.to_string();
    assert!(message.contains("ORBIT_OPERATOR"), "{message}");
    assert!(message.contains("machine_id"), "{message}");
}

#[test]
fn a_zero_concurrency_pull_is_refused_before_the_owner_is_probed() {
    if !enter_isolated_child(
        "operator_remedies::a_zero_concurrency_pull_is_refused_before_the_owner_is_probed",
    ) {
        return;
    }
    let transport = Arc::new(RecordingTransport {
        calls: Mutex::new(Vec::new()),
        unknown_selector: false,
    });
    let replica = replica_of_owner(transport.clone());
    let selector = own_selector(&replica);
    let error = replica
        .submit_workspace_pull_run(
            WorkspacePullRequest {
                selector: &selector,
                for_seconds: None,
                max_active_leaf_runs: Some(0),
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("zero slots is refused");
    assert!(error.to_string().contains("--concurrency"), "{error}");
    assert!(
        transport.calls.lock().expect("calls lock").is_empty(),
        "input validation must not cost an owner round trip"
    );
}

#[test]
fn a_pull_whose_owner_is_not_a_configured_destination_names_the_destinations_file() {
    if !enter_isolated_child(
        "operator_remedies::a_pull_whose_owner_is_not_a_configured_destination_names_the_destinations_file",
    ) {
        return;
    }
    let transport = Arc::new(RecordingTransport {
        calls: Mutex::new(Vec::new()),
        unknown_selector: true,
    });
    let replica = replica_of_owner(transport);
    let selector = own_selector(&replica);
    let error = replica
        .submit_workspace_pull_run(
            WorkspacePullRequest {
                selector: &selector,
                for_seconds: None,
                max_active_leaf_runs: None,
                actor: None,
            },
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect_err("an unrouted owner is refused");
    assert!(matches!(error, OrbitError::UnknownSelector(_)), "{error}");
    assert!(
        error.to_string().contains("mcp-destinations.toml"),
        "an unknown owner destination must say where destinations are configured: {error}"
    );
}
