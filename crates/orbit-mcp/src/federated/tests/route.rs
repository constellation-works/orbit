//! Fail-closed routing against fake destinations: no SSH, no local catalog.

use super::super::host::FederatedMcpHost;
use super::super::probe::DestinationSnapshot;
use super::fixtures::{
    OWNER_MACHINE, REPLICA_MACHINE, ScriptedProbe, ScriptedToolResult, destination, workspace,
};
use crate::McpHost;
use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;

use serde_json::{Value, json};
use std::sync::Arc;

fn destinations() -> Vec<super::super::config::Destination> {
    vec![
        destination("orbit-owner", OWNER_MACHINE),
        destination("operator@orbit-replica", REPLICA_MACHINE),
        destination("orbit-down", "hm_down"),
    ]
}

fn owner_snapshot() -> DestinationSnapshot {
    DestinationSnapshot {
        crews: Default::default(),
        machine_id: OWNER_MACHINE.to_string(),
        workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        host: Default::default(),
    }
}

fn replica_snapshot() -> DestinationSnapshot {
    DestinationSnapshot {
        crews: Default::default(),
        machine_id: REPLICA_MACHINE.to_string(),
        workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        host: Default::default(),
    }
}

fn three_destination_probe() -> ScriptedProbe {
    ScriptedProbe::new()
        .answering(OWNER_MACHINE, owner_snapshot())
        .answering(REPLICA_MACHINE, replica_snapshot())
        .refusing(
            "hm_down",
            OrbitError::UnreachableDestination("hm_down: could not start SSH".to_string()),
        )
}

fn routed_mux() -> (FederatedMcpHost, super::fixtures::CallLog) {
    let probe = three_destination_probe();
    let log = probe.call_log();
    (FederatedMcpHost::new(destinations(), Arc::new(probe)), log)
}

fn call(host: &FederatedMcpHost, tool: &str, selector: &str) -> Result<Value, OrbitError> {
    host.call_tool(
        tool,
        json!({ "workspace": selector }),
        ToolSessionContext::default(),
    )
}

fn call_err(host: &FederatedMcpHost, tool: &str, selector: &str) -> OrbitError {
    call(host, tool, selector).expect_err("expected a routing failure")
}

#[test]
fn unknown_selectors_fail_before_any_destination() {
    let (host, log) = routed_mux();

    for selector in ["ws_orbit", "orbit-linux/ws_orbit", "hm_unknown/ws_orbit"] {
        let error = call_err(&host, "orbit.workflow.run.list", selector);
        assert!(
            matches!(error, OrbitError::UnknownSelector(_)),
            "{selector}: {error}"
        );
    }
    assert!(
        log.calls().is_empty(),
        "a token that is not a configured hm_*/ws_* must not touch a destination"
    );
}

#[test]
fn a_dispatched_call_whose_answer_is_lost_is_outcome_unknown_not_unreachable() {
    let probe = three_destination_probe().on_call(
        OWNER_MACHINE,
        "orbit.task.add",
        ScriptedToolResult::PostDispatchTimeout,
    );
    let log = probe.call_log();
    let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

    let error = call_err(&host, "orbit.task.add", "hm_owner/ws_orbit");
    assert!(
        matches!(error, OrbitError::OutcomeUnknown { .. }),
        "the mux must not fold a possibly-committed remote write into the delivery-miss class \
         a caller retries: {error}"
    );
    let calls = log.calls();
    assert_eq!(
        calls.len(),
        1,
        "the call was delivered once and is not retried here: {calls:?}"
    );
}
