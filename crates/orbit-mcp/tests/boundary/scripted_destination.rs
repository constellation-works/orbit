//! Scripted destinations for the federated mux. The probe stands in for SSH
//! so routing and lost-answer classification run without a live host.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_mcp::federated::{Destination, DestinationProbe, DestinationSnapshot, RoutedSession};
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde_json::Value;

pub(crate) const OWNER_MACHINE: &str = "hm_owner";
pub(crate) const REPLICA_MACHINE: &str = "hm_replica";

pub(crate) fn destination(ssh: &str, machine_id: &str) -> Destination {
    Destination::ssh(ssh, machine_id)
}

pub(crate) fn workspace(id: &str, owner_machine_id: Option<&str>) -> Workspace {
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

type RoutedCall = (String, String, Value);

#[derive(Clone)]
pub(crate) struct CallLog(Arc<Mutex<Vec<RoutedCall>>>);

impl CallLog {
    pub(crate) fn calls(&self) -> Vec<RoutedCall> {
        self.0.lock().expect("call log").clone()
    }
}

/// Canned destination `tools/call` result.
///
/// Unscripted tools echo the rewritten arguments so a test can see the bare
/// `ws_*`.
#[derive(Debug, Clone)]
pub(crate) enum ScriptedToolResult {
    /// The destination took the `tools/call` and then stopped answering.
    PostDispatchTimeout,
}

/// A probe with one canned outcome per destination `machine_id`.
pub(crate) struct ScriptedProbe {
    outcomes: HashMap<String, Result<DestinationSnapshot, OrbitError>>,
    calls: HashMap<String, HashMap<String, ScriptedToolResult>>,
    call_log: Arc<Mutex<Vec<RoutedCall>>>,
}

impl ScriptedProbe {
    pub(crate) fn new() -> Self {
        Self {
            outcomes: HashMap::new(),
            calls: HashMap::new(),
            call_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn answering(mut self, machine_id: &str, snapshot: DestinationSnapshot) -> Self {
        self.outcomes.insert(machine_id.to_string(), Ok(snapshot));
        self
    }

    pub(crate) fn refusing(mut self, machine_id: &str, error: OrbitError) -> Self {
        self.outcomes.insert(machine_id.to_string(), Err(error));
        self
    }

    pub(crate) fn on_call(
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

    pub(crate) fn call_log(&self) -> CallLog {
        CallLog(Arc::clone(&self.call_log))
    }
}

impl DestinationProbe for ScriptedProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        match self.outcomes.get(&destination.machine_id) {
            Some(Ok(snapshot)) => Ok(snapshot.clone()),
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
            Some(Ok(listed)) => Ok(Box::new(ScriptedRoute {
                machine_id: destination.machine_id.clone(),
                snapshot: listed.clone(),
                tools: canonical_advertised_names(),
                calls: self
                    .calls
                    .get(&destination.machine_id)
                    .cloned()
                    .unwrap_or_default(),
                log: Arc::clone(&self.call_log),
            })),
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

    fn supports_tool_argument(&mut self, name: &str, argument: &str) -> Result<bool, OrbitError> {
        if !self
            .tools
            .iter()
            .any(|tool| mcp_advertised_tool_name(tool) == mcp_advertised_tool_name(name))
        {
            return Ok(false);
        }
        Ok(orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?
            .iter()
            .any(|definition| {
                definition.schema.name == name
                    && definition
                        .schema
                        .parameters
                        .iter()
                        .any(|parameter| parameter.name == argument)
            }))
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        _session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.log.lock().expect("call log").push((
            self.machine_id.clone(),
            name.to_string(),
            arguments.clone(),
        ));
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
        }
    }
}

fn canonical_advertised_names() -> Vec<String> {
    orbit_mcp::canonical_mcp_tool_definitions()
        .map(|definitions| {
            definitions
                .into_iter()
                .map(|definition| mcp_advertised_tool_name(&definition.schema.name))
                .collect()
        })
        .unwrap_or_default()
}
