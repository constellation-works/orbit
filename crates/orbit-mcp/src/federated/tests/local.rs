//! Implicit local membership: listing, routing, and collapse of an explicit
//! SSH row that names the accepting machine.

use std::sync::Arc;
use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, McpToolDefinition, ToolSessionContext};
use serde_json::{Value, json};

use super::super::config::{
    Destination, DestinationsFile, RemoteDestination, federated_membership,
};
use super::super::host::{FEDERATED_WORKSPACE_LIST_TOOL, FederatedMcpHost};
use super::super::probe::{
    CompositeDestinationProbe, DestinationProbe, DestinationSnapshot, InProcessDestinationProbe,
    RoutedSession,
};
use super::fixtures::{
    OWNER_MACHINE, REPLICA_MACHINE, ScriptedProbe, destination, local_destination, workspace,
};
use crate::McpHost;

struct RecordingLocalHost {
    machine_id: String,
    workspaces: Vec<orbit_types::workspace::Workspace>,
    calls: Mutex<Vec<(String, Value, ToolSessionContext)>>,
}

impl RecordingLocalHost {
    fn new(machine_id: &str) -> Self {
        Self {
            machine_id: machine_id.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(machine_id))],
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl McpHost for RecordingLocalHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        crate::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if name == crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL {
            return Ok(json!({
                "machine_id": self.machine_id,
                "workspaces": self.workspaces,
            }));
        }
        self.calls.lock().expect("call log").push((
            name.to_string(),
            input.clone(),
            session_context,
        ));
        Ok(input)
    }
}

struct PanickingProbe;

impl DestinationProbe for PanickingProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        panic!(
            "SSH probe must not run for local destination {}",
            destination.machine_id
        );
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        panic!(
            "SSH session must not open for local destination {}",
            destination.machine_id
        );
    }
}

fn list(host: &FederatedMcpHost) -> Vec<Value> {
    let listed = host
        .call_tool(
            FEDERATED_WORKSPACE_LIST_TOOL,
            Value::Null,
            ToolSessionContext::default(),
        )
        .expect("federated list");
    listed["workspaces"]
        .as_array()
        .expect("workspace rows")
        .clone()
}

fn local_only_mux() -> FederatedMcpHost {
    let inner = Arc::new(RecordingLocalHost::new(OWNER_MACHINE));
    let context = ToolSessionContext {
        process_machine_id: Some(OWNER_MACHINE.to_string()),
        process_machine_name: Some("local-host".to_string()),
        transport: Some(orbit_types::tool::McpTransport::Local),
        ..ToolSessionContext::default()
    };
    let probe = CompositeDestinationProbe::new(
        Arc::new(InProcessDestinationProbe::new(inner, context)),
        Arc::new(PanickingProbe),
    );
    FederatedMcpHost::new(
        vec![local_destination(OWNER_MACHINE, "local-host")],
        Arc::new(probe),
    )
}

fn recording_local_mux() -> (Arc<FederatedMcpHost>, Arc<RecordingLocalHost>) {
    let inner = Arc::new(RecordingLocalHost::new(OWNER_MACHINE));
    let context = ToolSessionContext {
        caller_machine_id: Some(OWNER_MACHINE.to_string()),
        caller_machine_name: Some("local-host".to_string()),
        process_machine_id: Some(OWNER_MACHINE.to_string()),
        process_machine_name: Some("local-host".to_string()),
        transport: Some(orbit_types::tool::McpTransport::Local),
        effective_capabilities: std::collections::BTreeSet::from([McpCapability::Agent]),
        ..ToolSessionContext::default()
    };
    let host = FederatedMcpHost::new(
        vec![local_destination(OWNER_MACHINE, "local-host")],
        Arc::new(CompositeDestinationProbe::new(
            Arc::new(InProcessDestinationProbe::new(
                Arc::clone(&inner) as Arc<dyn McpHost>,
                context,
            )),
            Arc::new(PanickingProbe),
        )),
    );
    (Arc::new(host), inner)
}

#[test]
fn local_only_membership_lists_the_accepting_machines_workspaces() {
    let rows = list(&local_only_mux());

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["machine_id"], OWNER_MACHINE);
    assert_eq!(rows[0]["machine_name"], "local-host");
    assert_eq!(rows[0]["selector"], format!("{OWNER_MACHINE}/ws_orbit"));
    assert_eq!(rows[0]["reachability"], "reachable");
    assert_eq!(rows[0]["checkout_health"], "active");
    assert_eq!(rows[0]["capabilities"], json!(["control_plane", "execute"]));
    assert_eq!(rows[0]["id"], "ws_orbit");
}

#[test]
fn mixed_membership_lists_local_then_configured_remotes() {
    let destinations = federated_membership(
        OWNER_MACHINE,
        "local-host",
        DestinationsFile {
            destinations: vec![
                RemoteDestination {
                    ssh: "operator@orbit-replica".to_string(),
                    machine_id: REPLICA_MACHINE.to_string(),
                },
                RemoteDestination {
                    ssh: "orbit-down".to_string(),
                    machine_id: "hm_down".to_string(),
                },
            ],
        },
    );
    let local = ScriptedProbe::new().answering(
        OWNER_MACHINE,
        DestinationSnapshot {
            machine_id: OWNER_MACHINE.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        },
    );
    let remote = ScriptedProbe::new()
        .answering(
            REPLICA_MACHINE,
            DestinationSnapshot {
                machine_id: REPLICA_MACHINE.to_string(),
                workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
            },
        )
        .refusing(
            "hm_down",
            OrbitError::UnreachableDestination("hm_down: could not start SSH".to_string()),
        );
    let host = FederatedMcpHost::new(
        destinations,
        Arc::new(CompositeDestinationProbe::new(
            Arc::new(local),
            Arc::new(remote),
        )),
    );

    let rows = list(&host);
    assert_eq!(
        rows.iter()
            .map(|row| row["machine_id"].as_str().expect("machine_id"))
            .collect::<Vec<_>>(),
        [OWNER_MACHINE, REPLICA_MACHINE, "hm_down"]
    );
    assert_eq!(rows[0]["machine_name"], "local-host");
    assert_eq!(rows[0]["selector"], format!("{OWNER_MACHINE}/ws_orbit"));
    assert_eq!(rows[1]["machine_name"], "operator@orbit-replica");
    assert_eq!(rows[2]["reachability"], "unreachable");
}

#[test]
fn an_explicit_local_ssh_row_does_not_duplicate_the_local_descriptor() {
    let destinations = federated_membership(
        OWNER_MACHINE,
        "local-host",
        DestinationsFile {
            destinations: vec![RemoteDestination {
                ssh: "localhost".to_string(),
                machine_id: OWNER_MACHINE.to_string(),
            }],
        },
    );
    assert_eq!(destinations.len(), 1);
    assert!(destinations[0].is_local());

    let probe = CompositeDestinationProbe::new(
        Arc::new(ScriptedProbe::new().answering(
            OWNER_MACHINE,
            DestinationSnapshot {
                machine_id: OWNER_MACHINE.to_string(),
                workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
            },
        )),
        Arc::new(PanickingProbe),
    );
    let host = FederatedMcpHost::new(destinations, Arc::new(probe));
    let rows = list(&host);

    assert_eq!(rows.len(), 1, "exactly one route for the local machine");
    assert_eq!(rows[0]["machine_id"], OWNER_MACHINE);
    assert_eq!(rows[0]["machine_name"], "local-host");
    assert_eq!(rows[0]["selector"], format!("{OWNER_MACHINE}/ws_orbit"));
}

#[test]
fn a_copied_local_selector_is_delivered_in_process_without_ssh() {
    let (host, inner) = recording_local_mux();
    let call_context = ToolSessionContext {
        process_machine_id: Some("hm_spoofed".to_string()),
        process_machine_name: Some("spoofed-host".to_string()),
        transport: Some(orbit_types::tool::McpTransport::SshMcp),
        trace_id: Some("trace-current-call".to_string()),
        effective_capabilities: std::collections::BTreeSet::from([McpCapability::Operator]),
        self_reported_actor: Some("codex".to_string()),
        ..ToolSessionContext::default()
    };

    let result = host
        .call_tool(
            "orbit.crew.list",
            json!({ "workspace": format!("{OWNER_MACHINE}/ws_orbit") }),
            call_context,
        )
        .expect("local crew.list");
    assert_eq!(result["workspace"], "ws_orbit");

    let calls = inner.calls.lock().expect("call log");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "orbit.crew.list");
    assert_eq!(calls[0].1["workspace"], "ws_orbit");
    assert_eq!(
        calls[0].2.process_machine_id.as_deref(),
        Some(OWNER_MACHINE)
    );
    assert_eq!(
        calls[0].2.process_machine_name.as_deref(),
        Some("local-host")
    );
    assert_eq!(calls[0].2.caller_machine_id.as_deref(), Some(OWNER_MACHINE));
    assert_eq!(
        calls[0].2.caller_machine_name.as_deref(),
        Some("local-host")
    );
    assert_eq!(
        calls[0].2.transport,
        Some(orbit_types::tool::McpTransport::Local)
    );
    assert_eq!(
        calls[0].2.effective_capabilities,
        std::collections::BTreeSet::from([McpCapability::Agent])
    );
    assert_eq!(calls[0].2.trace_id.as_deref(), Some("trace-current-call"));
    assert_eq!(calls[0].2.self_reported_actor.as_deref(), Some("codex"));
}

#[test]
fn concurrent_local_routes_keep_each_calls_audit_evidence_isolated() {
    let (host, inner) = recording_local_mux();

    std::thread::scope(|scope| {
        for (trace_id, actor) in [("trace-a", "codex"), ("trace-b", "claude-code")] {
            let host = Arc::clone(&host);
            scope.spawn(move || {
                host.call_tool(
                    "orbit.crew.list",
                    json!({ "workspace": format!("{OWNER_MACHINE}/ws_orbit") }),
                    ToolSessionContext {
                        trace_id: Some(trace_id.to_string()),
                        self_reported_actor: Some(actor.to_string()),
                        ..ToolSessionContext::default()
                    },
                )
                .expect("concurrent local route");
            });
        }
    });

    let calls = inner.calls.lock().expect("call log");
    assert_eq!(calls.len(), 2);
    let mut evidence = calls
        .iter()
        .map(|(_, _, context)| {
            (
                context.trace_id.as_deref().expect("trace").to_string(),
                context
                    .self_reported_actor
                    .as_deref()
                    .expect("actor")
                    .to_string(),
            )
        })
        .collect::<Vec<_>>();
    evidence.sort();
    assert_eq!(
        evidence,
        [
            ("trace-a".to_string(), "codex".to_string()),
            ("trace-b".to_string(), "claude-code".to_string())
        ]
    );
    for (_, _, context) in calls.iter() {
        assert_eq!(context.process_machine_id.as_deref(), Some(OWNER_MACHINE));
        assert_eq!(context.process_machine_name.as_deref(), Some("local-host"));
        assert_eq!(
            context.effective_capabilities,
            std::collections::BTreeSet::from([McpCapability::Agent])
        );
    }
}

#[test]
fn mixed_routing_keeps_remote_calls_on_the_ssh_probe() {
    let local = ScriptedProbe::new().answering(
        OWNER_MACHINE,
        DestinationSnapshot {
            machine_id: OWNER_MACHINE.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        },
    );
    let remote = ScriptedProbe::new().answering(
        REPLICA_MACHINE,
        DestinationSnapshot {
            machine_id: REPLICA_MACHINE.to_string(),
            workspaces: vec![workspace("ws_orbit", Some(OWNER_MACHINE))],
        },
    );
    let remote_log = remote.call_log();
    let local_log = local.call_log();
    let host = FederatedMcpHost::new(
        vec![
            local_destination(OWNER_MACHINE, "local-host"),
            destination("operator@orbit-replica", REPLICA_MACHINE),
        ],
        Arc::new(CompositeDestinationProbe::new(
            Arc::new(local),
            Arc::new(remote),
        )),
    );

    host.call_tool(
        "orbit.crew.list",
        json!({ "workspace": format!("{OWNER_MACHINE}/ws_orbit") }),
        ToolSessionContext::default(),
    )
    .expect("local route");
    host.call_tool(
        "orbit.task.show",
        json!({ "workspace": format!("{REPLICA_MACHINE}/ws_orbit") }),
        ToolSessionContext::default(),
    )
    .expect("remote route");

    assert_eq!(local_log.calls().len(), 1);
    assert_eq!(local_log.calls()[0].machine_id, OWNER_MACHINE);
    assert_eq!(remote_log.calls().len(), 1);
    assert_eq!(remote_log.calls()[0].machine_id, REPLICA_MACHINE);
}

#[test]
fn worker_context_survives_local_route_without_operator_elevation() {
    let (host, inner) = recording_local_mux();
    let binding = orbit_types::tool::WorkerInvocation {
        owner_machine_id: OWNER_MACHINE.into(),
        owner_workspace_id: "ws_orbit".into(),
        owner_destination: format!("{OWNER_MACHINE}/ws_orbit"),
        task_id: "task".into(),
        claim_id: "claim".into(),
        execution: orbit_types::task::ExecutionLocation {
            machine_id: REPLICA_MACHINE.into(),
            machine_name: None,
        },
        bound_run_id: "immutable-leaf".into(),
    };
    let session = ToolSessionContext {
        workspace: Some(binding.owner_destination.clone()),
        worker_invocation: Some(binding.clone()),
        effective_capabilities: [McpCapability::Agent, McpCapability::Operator].into(),
        ..Default::default()
    };
    host.call_tool("orbit.task.show", json!({"id":"task"}), session.clone())
        .expect("route");
    let calls = inner.calls.lock().expect("calls");
    assert_eq!(calls[0].2.worker_invocation.as_ref(), Some(&binding));
    assert!(
        !calls[0]
            .2
            .effective_capabilities
            .contains(&McpCapability::Operator)
    );
    drop(calls);
    assert!(
        host.call_tool(
            "orbit.task.show",
            json!({"id":"task", "workspace": format!("{REPLICA_MACHINE}/ws_orbit")}),
            session
        )
        .is_err()
    );
    assert_eq!(inner.calls.lock().expect("calls").len(), 1);
}
