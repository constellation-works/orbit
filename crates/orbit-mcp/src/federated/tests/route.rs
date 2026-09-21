//! Fail-closed routing against fake destinations: no SSH, no local catalog.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workspace::WorkspaceStatus;
use serde_json::{Value, json};

use super::super::host::FederatedMcpHost;
use super::super::probe::DestinationSnapshot;
use super::fixtures::{
    OWNER_MACHINE, REPLICA_MACHINE, ScriptedProbe, ScriptedToolResult, destination, workspace,
};
use crate::McpHost;

fn destinations() -> Vec<super::super::config::Destination> {
    vec![
        destination("orbit-owner", OWNER_MACHINE),
        destination("operator@orbit-replica", REPLICA_MACHINE),
        destination("orbit-down", "hm_down"),
    ]
}

fn owner_snapshot() -> DestinationSnapshot {
    DestinationSnapshot {
        machine_id: OWNER_MACHINE.to_string(),
        workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
    }
}

fn replica_snapshot() -> DestinationSnapshot {
    DestinationSnapshot {
        machine_id: REPLICA_MACHINE.to_string(),
        workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
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
        let error = call_err(&host, "orbit.crew.list", selector);
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
fn a_down_destination_is_unreachable_not_capability_or_stale() {
    let (host, log) = routed_mux();

    let error = call_err(&host, "orbit.task.add", "hm_down/ws_orbit");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "{error}"
    );
    assert!(
        log.calls().is_empty(),
        "unreachable must win over capability and stale: {calls:?}",
        calls = log.calls()
    );
}

#[test]
fn a_missing_workspace_on_a_live_host_is_a_stale_route() {
    let (host, log) = routed_mux();

    let error = call_err(&host, "orbit.crew.list", "hm_owner/ws_missing");
    assert!(matches!(error, OrbitError::StaleRoute(_)), "{error}");
    assert!(log.calls().is_empty(), "stale routes are not delivered");
}

#[test]
fn a_missing_repo_root_is_an_unhealthy_checkout() {
    let mut broken = workspace("ws_broken", Some(OWNER_MACHINE));
    broken.status = WorkspaceStatus::Invalid;
    let probe = ScriptedProbe::new().answering(
        OWNER_MACHINE,
        DestinationSnapshot {
            machine_id: OWNER_MACHINE.to_string(),
            workspaces: vec![broken],
        },
    );
    let log = probe.call_log();
    let host = FederatedMcpHost::new(
        vec![destination("orbit-owner", OWNER_MACHINE)],
        Arc::new(probe),
    );

    let error = call_err(&host, "orbit.crew.list", "hm_owner/ws_broken");
    assert!(matches!(error, OrbitError::UnhealthyCheckout(_)), "{error}");
    assert!(
        log.calls().is_empty(),
        "unhealthy checkouts are not delivered"
    );
}

#[test]
fn a_tool_missing_from_the_destination_surface_is_not_on_this_host() {
    let probe = three_destination_probe()
        .advertising(OWNER_MACHINE, &["orbit_workspace_list", "orbit_crew_list"]);
    let log = probe.call_log();
    let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

    let error = call_err(&host, "orbit.task.show", "hm_owner/ws_orbit");
    assert!(matches!(error, OrbitError::ToolNotOnThisHost(_)), "{error}");
    assert!(
        log.calls().is_empty(),
        "mixed-version misses must not be sent as a destination tools/call"
    );
}

#[test]
fn replica_task_add_returns_destination_capability_refused() {
    let probe = three_destination_probe().on_call(
        REPLICA_MACHINE,
        "orbit.task.add",
        ScriptedToolResult::RemoteTool {
            code: "capability_refused".to_string(),
            message: "this checkout does not hold the 'control_plane' capability class".to_string(),
        },
    );
    let log = probe.call_log();
    let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

    let error = call_err(&host, "orbit.task.add", "hm_replica/ws_orbit");
    match error {
        OrbitError::RemoteTool { code, payload, .. } => {
            assert_eq!(code, "capability_refused");
            assert_eq!(payload["code"], "capability_refused");
        }
        other => panic!("destination refusal must stay RemoteTool, got {other}"),
    }
    let calls = log.calls();
    assert_eq!(calls.len(), 1, "no owner failover: {calls:?}");
    assert_eq!(calls[0].machine_id, REPLICA_MACHINE);
    assert_eq!(calls[0].tool, "orbit.task.add");
    assert_eq!(calls[0].arguments["workspace"], "ws_orbit");
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

#[test]
fn a_copied_selector_round_trips_to_the_encoded_host() {
    let (host, log) = routed_mux();

    let owner = call(&host, "orbit.crew.list", "hm_owner/ws_orbit").expect("owner crew.list");
    assert_eq!(owner["workspace"], "ws_orbit");

    let shown = call(&host, "orbit.task.show", "hm_replica/ws_orbit").expect("replica task.show");
    assert_eq!(shown["workspace"], "ws_orbit");

    let calls = log.calls();
    assert_eq!(
        calls
            .iter()
            .map(|call| (call.machine_id.as_str(), call.tool.as_str()))
            .collect::<Vec<_>>(),
        [
            (OWNER_MACHINE, "orbit.crew.list"),
            (REPLICA_MACHINE, "orbit.task.show"),
        ]
    );
    assert!(
        calls
            .iter()
            .all(|call| call.arguments["workspace"] == "ws_orbit"),
        "v1 destinations receive the bare workspace id: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .all(|call| call.arguments["workspace"] != "hm_owner/ws_orbit"
                && call.arguments["workspace"] != "hm_replica/ws_orbit"),
        "the host-qualified token must not be forwarded: {calls:?}"
    );
}

#[test]
fn routing_does_not_reuse_list_health() {
    let probe = ScriptedProbe::new()
        .answering(OWNER_MACHINE, owner_snapshot())
        .route_snapshot(
            OWNER_MACHINE,
            DestinationSnapshot {
                machine_id: OWNER_MACHINE.to_string(),
                workspaces: Vec::new(),
            },
        );
    let host = FederatedMcpHost::new(
        vec![destination("orbit-owner", OWNER_MACHINE)],
        Arc::new(probe),
    );

    let listed = host
        .call_tool(
            "orbit.workspace.list",
            Value::Null,
            ToolSessionContext::default(),
        )
        .expect("list");
    assert_eq!(
        listed["workspaces"][0]["selector"],
        format!("{OWNER_MACHINE}/ws_orbit"),
        "the list still shows the workspace"
    );

    let error = call_err(&host, "orbit.crew.list", "hm_owner/ws_orbit");
    assert!(
        matches!(error, OrbitError::StaleRoute(_)),
        "live delivery, not cached list health: {error}"
    );
}

#[test]
fn a_session_announced_selector_routes_like_a_call_argument() {
    let (host, log) = routed_mux();
    let context = ToolSessionContext {
        workspace: Some(format!("{OWNER_MACHINE}/ws_orbit")),
        ..ToolSessionContext::default()
    };

    let result = host
        .call_tool("orbit.crew.list", json!({}), context)
        .expect("session selector");
    assert_eq!(result["workspace"], "ws_orbit");
    assert_eq!(log.calls()[0].machine_id, OWNER_MACHINE);
}

#[test]
fn a_session_defaulted_bare_workspace_id_is_unknown_before_forwarding() {
    let (host, log) = routed_mux();
    let context = ToolSessionContext {
        workspace: Some("ws_orbit".to_string()),
        ..ToolSessionContext::default()
    };

    let error = host
        .call_tool("orbit.crew.list", json!({}), context)
        .expect_err("bare session default must not route");
    assert!(
        matches!(error, OrbitError::UnknownSelector(_)),
        "session-defaulted v1 form is unknown_selector: {error}"
    );
    assert!(
        log.calls().is_empty(),
        "a bare ws_* must not touch a destination, including when initialize injected it"
    );
}

#[test]
fn federated_task_show_without_a_machine_qualified_selector_is_refused() {
    let (host, log) = routed_mux();

    let omitted = host
        .call_tool(
            "orbit.task.show",
            json!({ "id": "ORB-00001" }),
            ToolSessionContext::default(),
        )
        .expect_err("federated task.show does not inherit id-only default");
    assert!(
        matches!(omitted, OrbitError::InvalidInput(_)),
        "omitting the selector is refused: {omitted}"
    );

    let bare = call_err(&host, "orbit.task.show", "ws_orbit");
    assert!(
        matches!(bare, OrbitError::UnknownSelector(_)),
        "a bare ws_* on task.show is unknown_selector: {bare}"
    );
    assert!(
        log.calls().is_empty(),
        "refused federated task.show must not forward: {calls:?}",
        calls = log.calls()
    );
}

#[test]
fn an_identity_mismatch_on_the_live_route_is_unreachable() {
    let probe = ScriptedProbe::new()
        .answering(OWNER_MACHINE, owner_snapshot())
        .route_snapshot(
            OWNER_MACHINE,
            DestinationSnapshot {
                machine_id: "hm_impostor".to_string(),
                workspaces: vec![workspace("ws_orbit", Some("hm_impostor"))],
            },
        );
    let log = probe.call_log();
    let host = FederatedMcpHost::new(
        vec![destination("orbit-owner", OWNER_MACHINE)],
        Arc::new(probe),
    );

    let error = call_err(&host, "orbit.crew.list", "hm_owner/ws_orbit");
    assert!(
        matches!(error, OrbitError::UnreachableDestination(_)),
        "{error}"
    );
    assert!(log.calls().is_empty());
}
