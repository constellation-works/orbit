//! Fail-closed federated routing through the exported mux.
//!
//! `FederatedMcpHost` and `DestinationProbe` are the public seams. The
//! scripted probe supplies destination answers without SSH.

use std::sync::Arc;

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_mcp::McpHost;
use orbit_mcp::federated::{Destination, DestinationSnapshot, FederatedMcpHost};
use orbit_registry::hosts::{HostEntry, TaskPrefixTable};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::scripted_destination::{
    OWNER_MACHINE, REPLICA_MACHINE, ScriptedProbe, ScriptedToolResult, destination, workspace,
};

mod workspace_route {
    use super::*;

    fn destinations() -> Vec<Destination> {
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

    fn routed_mux() -> (FederatedMcpHost, crate::scripted_destination::CallLog) {
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

    /// A destination reached whose store stayed locked decided nothing; it
    /// is not a host the caller lost. The mux keeps its `lock_busy` class, so
    /// a bound worker retries the read and a follower drain backs off rather
    /// than suppressing its own host.
    #[test]
    fn a_destination_whose_store_stayed_locked_is_lock_busy_through_the_mux() {
        let probe = three_destination_probe().store_locked(OWNER_MACHINE);
        let log = probe.call_log();
        let host = FederatedMcpHost::new(destinations(), Arc::new(probe));

        let error = call_err(&host, "orbit.task.show", "hm_owner/ws_orbit");
        assert!(error.is_lock_busy(), "{error:?}");
        assert!(
            !matches!(error, OrbitError::UnreachableDestination(_)),
            "{error:?}"
        );
        assert!(log.calls().is_empty(), "nothing was dispatched");
    }
}

mod task_route {
    use super::*;

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

    fn routed(
        mirror: Option<&'static str>,
    ) -> (FederatedMcpHost, crate::scripted_destination::CallLog) {
        let probe = probe();
        let log = probe.call_log();
        let mut host = FederatedMcpHost::new(destinations(), Arc::new(probe))
            .with_task_prefix_routing(table());
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
}
