//! Short-lived SSH destination probes and routed MCP sessions.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use serde_json::{Value, json};

use super::super::config::Destination;
use super::contracts::{DestinationProbe, DestinationSnapshot, RoutedSession};
use super::discovery::unreachable;
use super::local::crews_arguments;
use super::session::DestinationSession;
use crate::remote::McpSessionAuthority;

/// The production probe: one short-lived SSH-hosted MCP session per call.
pub struct SshDestinationProbe {
    caller_machine_id: String,
    probe_timeout: Duration,
    delivery_timeout: Duration,
    /// The mux's orchestrator attribution default, carried into each
    /// destination's own argv [ORB-11313]. A routed session forwards no
    /// session context of its own, so this travels the same way the caller
    /// machine identity does.
    orchestrator: Option<String>,
    /// Authority the mux was started with, asked of every destination it
    /// opens [ORB-12564]. It travels in the same argv for the same reason the
    /// orchestrator does, and unlike the orchestrator it is a grant.
    authority: McpSessionAuthority,
}

impl SshDestinationProbe {
    /// `probe_timeout` bounds the phases that decide the route; a routed
    /// `tools/call` is re-stamped with `delivery_timeout` at dispatch, so the
    /// two are independent rather than shares of one session budget.
    pub fn new(
        caller_machine_id: String,
        probe_timeout: Duration,
        delivery_timeout: Duration,
        orchestrator: Option<String>,
        authority: McpSessionAuthority,
    ) -> Self {
        Self {
            caller_machine_id,
            probe_timeout,
            delivery_timeout,
            orchestrator,
            authority,
        }
    }
}

impl DestinationProbe for SshDestinationProbe {
    fn open_internal_drain_route(
        &self,
        destination: &Destination,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let child = spawn_destination_session(
            destination,
            &self.caller_machine_id,
            self.orchestrator.as_deref(),
            McpSessionAuthority::Agent,
            true,
            false,
        )?;
        let mut session =
            DestinationSession::start(destination.clone(), child, self.probe_timeout)?;
        session.handshake_with_worker(None)?;
        Ok(Box::new(SshRoutedSession::new(
            session,
            self.delivery_timeout,
        )))
    }
    fn open_worker_route(
        &self,
        destination: &Destination,
        context: &ToolSessionContext,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        Ok(Box::new(SshRoutedSession::new(
            self.start_worker_session(
                destination,
                context.worker_invocation.as_ref(),
                context.worker_host_call,
            )?,
            self.delivery_timeout,
        )))
    }

    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        let mut session = self.start_session(destination)?;
        session.discover_workspaces(json!({}))
    }

    fn probe_with_crews(
        &self,
        destination: &Destination,
    ) -> Result<DestinationSnapshot, OrbitError> {
        let mut session = self.start_session(destination)?;
        session.discover_workspaces(crews_arguments())
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        Ok(Box::new(SshRoutedSession::new(
            self.start_session(destination)?,
            self.delivery_timeout,
        )))
    }
}

impl SshDestinationProbe {
    fn start_session(&self, destination: &Destination) -> Result<DestinationSession, OrbitError> {
        self.start_worker_session(destination, None, false)
    }

    fn start_worker_session(
        &self,
        destination: &Destination,
        binding: Option<&orbit_types::tool::WorkerInvocation>,
        host_call: bool,
    ) -> Result<DestinationSession, OrbitError> {
        let child = spawn_destination_session(
            destination,
            &self.caller_machine_id,
            self.orchestrator.as_deref(),
            if binding.is_some() {
                McpSessionAuthority::Agent
            } else {
                self.authority
            },
            false,
            host_call,
        )?;
        // The session is one process; the guard ends it on every path,
        // including the timeout path where the child is still mid-answer.
        let mut session =
            DestinationSession::start(destination.clone(), child, self.probe_timeout)?;
        session.handshake_with_worker(binding)?;
        Ok(session)
    }
}

/// Production routed session: one SSH child, several MCP requests, then drop.
pub(super) struct SshRoutedSession {
    session: DestinationSession,
    snapshot: Option<DestinationSnapshot>,
    tools: Option<Vec<String>>,
    tool_schemas: std::collections::HashMap<String, Value>,
    /// The budget the tool call itself gets, stamped at dispatch rather than
    /// at session start.
    delivery_timeout: Duration,
}

impl SshRoutedSession {
    pub(super) fn new(session: DestinationSession, delivery_timeout: Duration) -> Self {
        Self {
            session,
            snapshot: None,
            tools: None,
            tool_schemas: std::collections::HashMap::new(),
            delivery_timeout,
        }
    }
}

impl RoutedSession for SshRoutedSession {
    fn internal_drain_protocol(&mut self) -> Result<bool, OrbitError> {
        let response = self
            .session
            .request_probe(crate::internal_drain::PREFLIGHT_METHOD, json!({}))?;
        Ok(response["result"]["protocol"].as_u64() == Some(crate::INTERNAL_DRAIN_PROTOCOL))
    }

    fn call_internal_drain(
        &mut self,
        name: &str,
        arguments: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if context.worker_invocation.is_some() || self.session.worker_invocation.is_some() {
            return Err(crate::internal_drain::refusal());
        }
        self.session.restart_budget(self.delivery_timeout);
        self.session.call_internal_drain(name, arguments)
    }
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        if let Some(snapshot) = &self.snapshot {
            return Ok(snapshot.clone());
        }
        let snapshot = self.session.discover_workspaces(json!({}))?;
        self.snapshot = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        if let Some(tools) = &self.tools {
            return Ok(tools.clone());
        }
        let definitions = self.session.list_tool_definitions()?;
        let tools = definitions
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        self.tool_schemas = definitions
            .iter()
            .filter_map(|tool| {
                tool["name"]
                    .as_str()
                    .map(|name| (name.to_string(), tool["inputSchema"].clone()))
            })
            .collect();
        self.tools = Some(tools.clone());
        Ok(tools)
    }
    fn supports_tool_argument(&mut self, name: &str, argument: &str) -> Result<bool, OrbitError> {
        self.advertised_tools()?;
        Ok(self
            .tool_schemas
            .get(&mcp_advertised_tool_name(name))
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.get(argument))
            .is_some())
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if session_context.worker_invocation != self.session.worker_invocation {
            return Err(OrbitError::PolicyDenied(
                "SSH worker binding mismatch".into(),
            ));
        }
        // Classification is finished, so the tool's own budget starts here:
        // the SSH setup, handshake, discovery, and `tools/list` round trips
        // that chose this destination must not shorten it.
        self.session.restart_budget(self.delivery_timeout);
        self.session.call_tool(name, arguments)
    }
}

/// Start the destination's MCP server over SSH with piped stdio.
///
/// `-T` and the remote argv are the v1 proxy's, so a destination sees exactly
/// the session shape it already supports. Only the stdio wiring differs: the
/// proxy inherits it for byte transparency, while the mux is itself the client
/// and must own both ends.
fn spawn_destination_session(
    destination: &Destination,
    caller_machine_id: &str,
    orchestrator: Option<&str>,
    authority: McpSessionAuthority,
    internal: bool,
    worker_host: bool,
) -> Result<Child, OrbitError> {
    let ssh = destination.ssh_target().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "local destination '{}' cannot be opened over SSH",
            destination.machine_id
        ))
    })?;
    let mut remote =
        crate::remote::remote_serve_command(caller_machine_id, orchestrator, authority);
    if internal {
        remote.push_str(" --internal-drain");
    }
    if worker_host {
        remote.push_str(" --worker-host");
    }
    Command::new("ssh")
        .arg("-T")
        .arg("--")
        .arg(ssh)
        .arg(remote)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The destination's logs are its own; folding them into this process's
        // stderr would interleave many hosts' output with no attribution. The
        // session keeps only their tail, so a host that never answered is
        // reported with ssh's own reason.
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| unreachable(destination, format!("could not start SSH: {error}")))
}
