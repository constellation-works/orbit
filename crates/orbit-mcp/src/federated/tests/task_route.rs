//! Id-only task calls route by the id's prefix, against fake destinations.

use super::super::config::Destination;
use super::super::host::FederatedMcpHost;
use super::super::probe::DestinationSnapshot;
use super::fixtures::{OWNER_MACHINE, ScriptedProbe, destination, workspace};
use crate::McpHost;
use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::hosts::{HostEntry, TaskPrefixTable};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};
use std::sync::Arc;

const LOCAL_MACHINE: &str = "hm_local";
const DOWN_MACHINE: &str = "hm_down";

fn snapshot(machine_id: &str, workspaces: &[(&str, &str)]) -> DestinationSnapshot {
    DestinationSnapshot {
        crews: Default::default(),
        machine_id: machine_id.to_string(),
        workspaces: workspaces
            .iter()
            .map(|(id, name)| {
                let mut row = workspace(id, Some(machine_id));
                row.name = name.to_string();
                row
            })
            .collect(),
        host: Default::default(),
    }
}

fn entry(name: &str, machine_id: &str, prefix: &str) -> HostEntry {
    HostEntry {
        name: name.to_string(),
        machine_id: machine_id.to_string(),
        ssh: name.to_string(),
        task_prefix: prefix.to_string(),
    }
}

fn destinations() -> Vec<Destination> {
    vec![
        Destination::local(LOCAL_MACHINE, "local-box"),
        destination("owner", OWNER_MACHINE),
        destination("down", DOWN_MACHINE),
    ]
}

fn table() -> TaskPrefixTable {
    TaskPrefixTable::new(
        Some("LB".to_string()),
        vec![
            entry("owner", OWNER_MACHINE, "OWN"),
            entry("down", DOWN_MACHINE, "DWN"),
        ],
    )
}

fn probe() -> ScriptedProbe {
    ScriptedProbe::new()
        .answering(
            LOCAL_MACHINE,
            snapshot(LOCAL_MACHINE, &[("ws_local", "local")]),
        )
        .answering(
            OWNER_MACHINE,
            snapshot(
                OWNER_MACHINE,
                &[
                    ("ws_orbit", "orbit"),
                    ("ws_twin1", "twin"),
                    ("ws_twin2", "twin"),
                ],
            ),
        )
        .refusing(
            DOWN_MACHINE,
            OrbitError::UnreachableDestination("hm_down: could not start SSH".into()),
        )
}

fn routed(mirror: Option<&'static str>) -> (FederatedMcpHost, super::fixtures::CallLog) {
    let probe = probe();
    let log = probe.call_log();
    let mut host =
        FederatedMcpHost::new(destinations(), Arc::new(probe)).with_task_prefix_routing(table());
    if let Some(workspace) = mirror {
        host = host.with_local_mirror_hint(Arc::new(move |_| Some(workspace.to_string())));
    }
    (host, log)
}

fn call(host: &FederatedMcpHost, tool: &str, input: Value) -> Result<Value, OrbitError> {
    host.call_tool(tool, input, ToolSessionContext::default())
}

#[test]
fn an_id_only_call_goes_to_the_host_its_prefix_names_without_a_selector() {
    let (host, log) = routed(None);

    let remote = call(
        &host,
        "orbit.task.update",
        json!({"id": "OWN-7", "comment": "hi"}),
    )
    .expect("remote prefix delivered");
    let local = call(&host, "orbit.task.show", json!({"id": "LB-3"})).expect("local prefix");

    assert_eq!(
        log.calls(),
        vec![
            (
                OWNER_MACHINE.to_string(),
                "orbit.task.update".to_string(),
                json!({"id": "OWN-7", "comment": "hi"}),
            ),
            (
                LOCAL_MACHINE.to_string(),
                "orbit.task.show".to_string(),
                json!({"id": "LB-3"}),
            ),
        ],
        "each id reaches only its prefix's host, and the destination resolves it itself"
    );
    assert_eq!(remote["id"], "OWN-7");
    assert_eq!(local["id"], "LB-3");
}

#[test]
fn an_explicit_selector_wins_over_the_prefix() {
    let (host, log) = routed(None);

    call(
        &host,
        "orbit.task.show",
        json!({"id": "OWN-7", "workspace": format!("{LOCAL_MACHINE}/ws_local")}),
    )
    .expect("selector route");

    let calls = log.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0].0, LOCAL_MACHINE,
        "the selector's host, not the prefix's"
    );
    assert_eq!(calls[0].2["workspace"], "ws_local");
}

#[test]
fn an_unregistered_prefix_is_refused_before_any_destination() {
    let (host, log) = routed(None);

    let error = call(&host, "orbit.task.show", json!({"id": "ZZZ-1"})).expect_err("refused");

    assert_eq!(
        error.host_registry_code(),
        Some(HostRegistryCode::UnknownTaskPrefix),
        "{error}"
    );
    assert!(log.calls().is_empty(), "routing never searches hosts");
}

#[test]
fn an_unreachable_holder_is_owner_unreachable_and_names_the_local_mirror() {
    let (host, log) = routed(Some("orbit-mirror"));

    let error = call(&host, "orbit.task.show", json!({"id": "DWN-4"})).expect_err("refused");

    assert_eq!(
        error.host_registry_code(),
        Some(HostRegistryCode::OwnerUnreachable),
        "{error}"
    );
    assert!(
        error.to_string().contains("--workspace orbit-mirror"),
        "the error says how to read the mirror explicitly: {error}"
    );
    assert!(
        log.calls().is_empty(),
        "no mirror fallback: {:?}",
        log.calls()
    );
}

#[test]
fn host_workspace_selector_copies_the_listed_selector() {
    let (host, _) = routed(None);

    assert_eq!(
        host.host_workspace_selector(OWNER_MACHINE, "orbit")
            .expect("by name"),
        format!("{OWNER_MACHINE}/ws_orbit")
    );
    assert_eq!(
        host.host_workspace_selector(OWNER_MACHINE, "ws_orbit")
            .expect("by id"),
        format!("{OWNER_MACHINE}/ws_orbit")
    );

    let stale = host
        .host_workspace_selector(OWNER_MACHINE, "missing")
        .expect_err("not listed");
    assert!(matches!(stale, OrbitError::StaleRoute(_)), "{stale}");
    assert!(
        stale.to_string().contains("orbit (ws_orbit)"),
        "stale_route lists what the host does list: {stale}"
    );
    let ambiguous = host
        .host_workspace_selector(OWNER_MACHINE, "twin")
        .expect_err("two matches");
    assert!(
        matches!(ambiguous, OrbitError::UnknownSelector(_)),
        "{ambiguous}"
    );
    let down = host
        .host_workspace_selector(DOWN_MACHINE, "orbit")
        .expect_err("no answer");
    assert!(
        matches!(down, OrbitError::UnreachableDestination(_)),
        "{down}"
    );
}
