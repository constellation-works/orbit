//! Fake destinations: a scripted probe stands in for every SSH session, so the
//! mux's projection, routing, and failure handling are exercised without a
//! live host.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::Value;

use super::super::config::Destination;
use super::super::probe::{DestinationProbe, DestinationSnapshot, RoutedSession};

pub(super) const OWNER_MACHINE: &str = "hm_owner";
pub(super) const REPLICA_MACHINE: &str = "hm_replica";

pub(super) fn destination(ssh: &str, machine_id: &str) -> Destination {
    Destination::ssh(ssh, machine_id)
}

pub(super) fn local_destination(machine_id: &str, machine_name: &str) -> Destination {
    Destination::local(machine_id, machine_name)
}

pub(super) fn workspace(id: &str, owner_machine_id: Option<&str>) -> Workspace {
    // A fixed timestamp keeps descriptor assertions stable.
    let at = Utc
        .with_ymd_and_hms(2026, 8, 23, 0, 0, 0)
        .single()
        .expect("fixture timestamp");
    Workspace {
        id: id.to_string(),
        name: id.trim_start_matches("ws_").to_string(),
        owner_machine_id: owner_machine_id.map(ToOwned::to_owned),
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: at,
        updated_at: at,
    }
}

/// How many times the mux actually reached out.
#[derive(Clone)]
pub(super) struct ProbeCallCounter(Arc<AtomicUsize>);

impl ProbeCallCounter {
    pub(super) fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// One delivered (or attempted) `tools/call` against a fake destination.
#[derive(Debug, Clone)]
pub(super) struct RoutedCall {
    pub machine_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Clone)]
pub(super) struct CallLog(Arc<Mutex<Vec<RoutedCall>>>);

impl CallLog {
    pub(super) fn calls(&self) -> Vec<RoutedCall> {
        self.0.lock().expect("call log").clone()
    }
}

/// Canned destination `tools/call` result.
///
/// Unscripted tools echo the rewritten arguments so tests can see the bare
/// `ws_*`.
#[derive(Debug, Clone)]
pub(super) enum ScriptedToolResult {
    RemoteTool {
        code: String,
        message: String,
    },
    /// The destination took the `tools/call` and then stopped answering: the
    /// mux wrote the request and never learned whether it ran.
    PostDispatchTimeout,
}

/// A probe with one canned outcome per destination `machine_id`.
pub(super) struct ScriptedProbe {
    outcomes: HashMap<String, Result<DestinationSnapshot, OrbitError>>,
    route_snapshots: HashMap<String, DestinationSnapshot>,
    tools: HashMap<String, Vec<String>>,
    calls: HashMap<String, HashMap<String, ScriptedToolResult>>,
    call_log: Arc<Mutex<Vec<RoutedCall>>>,
    probe_calls: Arc<AtomicUsize>,
}

impl ScriptedProbe {
    pub(super) fn new() -> Self {
        Self {
            outcomes: HashMap::new(),
            route_snapshots: HashMap::new(),
            tools: HashMap::new(),
            calls: HashMap::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
            probe_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(super) fn answering(mut self, machine_id: &str, snapshot: DestinationSnapshot) -> Self {
        self.outcomes.insert(machine_id.to_string(), Ok(snapshot));
        self
    }

    pub(super) fn refusing(mut self, machine_id: &str, error: OrbitError) -> Self {
        self.outcomes.insert(machine_id.to_string(), Err(error));
        self
    }

    /// Override the live route snapshot so list health cannot decide the call.
    pub(super) fn route_snapshot(
        mut self,
        machine_id: &str,
        snapshot: DestinationSnapshot,
    ) -> Self {
        self.route_snapshots
            .insert(machine_id.to_string(), snapshot);
        self
    }

    pub(super) fn advertising(mut self, machine_id: &str, tools: &[&str]) -> Self {
        self.tools.insert(
            machine_id.to_string(),
            tools.iter().map(|tool| (*tool).to_string()).collect(),
        );
        self
    }

    pub(super) fn on_call(
        mut self,
        machine_id: &str,
        tool: &str,
        result: ScriptedToolResult,
    ) -> Self {
        self.calls
            .entry(machine_id.to_string())
            .or_default()
            .insert(tool.to_string(), result);
        self
    }

    pub(super) fn call_counter(&self) -> ProbeCallCounter {
        ProbeCallCounter(Arc::clone(&self.probe_calls))
    }

    pub(super) fn call_log(&self) -> CallLog {
        CallLog(Arc::clone(&self.call_log))
    }
}

impl DestinationProbe for ScriptedProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        match self.outcomes.get(&destination.machine_id) {
            Some(Ok(snapshot)) => Ok(snapshot.clone()),
            // `OrbitError` is not `Clone`, so a refusal is restated rather than
            // copied; the variant is what the mux branches on.
            Some(Err(error)) => Err(OrbitError::UnreachableDestination(error.to_string())),
            None => Err(OrbitError::UnreachableDestination(format!(
                "{}: no scripted outcome",
                destination.machine_id
            ))),
        }
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        match self.outcomes.get(&destination.machine_id) {
            Some(Err(error)) => Err(OrbitError::UnreachableDestination(error.to_string())),
            None => Err(OrbitError::UnreachableDestination(format!(
                "{}: no scripted outcome",
                destination.machine_id
            ))),
            Some(Ok(listed)) => {
                let snapshot = self
                    .route_snapshots
                    .get(&destination.machine_id)
                    .cloned()
                    .unwrap_or_else(|| listed.clone());
                Ok(Box::new(ScriptedRoute {
                    machine_id: destination.machine_id.clone(),
                    snapshot,
                    tools: self
                        .tools
                        .get(&destination.machine_id)
                        .cloned()
                        .unwrap_or_else(canonical_advertised_names),
                    calls: self
                        .calls
                        .get(&destination.machine_id)
                        .cloned()
                        .unwrap_or_default(),
                    log: Arc::clone(&self.call_log),
                }))
            }
        }
    }
}

struct ScriptedRoute {
    machine_id: String,
    snapshot: DestinationSnapshot,
    tools: Vec<String>,
    calls: HashMap<String, ScriptedToolResult>,
    log: Arc<Mutex<Vec<RoutedCall>>>,
}

impl RoutedSession for ScriptedRoute {
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        Ok(self.snapshot.clone())
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        Ok(self.tools.clone())
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        _session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.log.lock().expect("call log").push(RoutedCall {
            machine_id: self.machine_id.clone(),
            tool: name.to_string(),
            arguments: arguments.clone(),
        });
        let scripted = self
            .calls
            .get(name)
            .or_else(|| self.calls.get(&mcp_advertised_tool_name(name)));
        match scripted {
            None => Ok(arguments),
            Some(ScriptedToolResult::PostDispatchTimeout) => Err(OrbitError::OutcomeUnknown {
                mcp_call_id: format!("{}/{name}", self.machine_id),
                message: "timed out waiting for 'tools/call'; the destination may have completed \
                          the call"
                    .to_string(),
            }),
            Some(ScriptedToolResult::RemoteTool { code, message }) => Err(OrbitError::RemoteTool {
                code: code.clone(),
                message: format!("{}: {message}", self.machine_id),
                payload: serde_json::json!({
                    "code": code,
                    "message": format!("{}: {message}", self.machine_id),
                }),
            }),
        }
    }
}

fn canonical_advertised_names() -> Vec<String> {
    crate::canonical_mcp_tool_definitions()
        .map(|definitions| {
            definitions
                .into_iter()
                .map(|definition| mcp_advertised_tool_name(&definition.schema.name))
                .collect()
        })
        .unwrap_or_default()
}
