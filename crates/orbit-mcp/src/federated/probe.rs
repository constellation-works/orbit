//! Live per-call probing and short-lived delivery to one configured destination.
//!
//! The mux answers `orbit.workspace.list` from what destinations say *now*, so
//! there is no health cache here and nothing is remembered between calls. A
//! routed workspace-scoped call opens one short-lived MCP session, confirms
//! the destination, and delivers that single `tools/call`. The client speaks
//! MCP over the same non-PTY SSH argv the v1 proxy uses.

use std::io::Read;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use orbit_types::workspace::Workspace;
use serde_json::{Value, json};

use super::config::Destination;

/// Lines the probe reader may queue ahead of the consumer before it blocks.
const PROBE_LINE_QUEUE: usize = 64;

/// Longest single line the probe reader accepts from a destination. Every
/// MCP message is one JSON line; anything past this is not a message.
const MAX_PROBE_LINE_BYTES: u64 = 64 * 1024 * 1024;

/// How long one destination gets to answer everything that decides *where* a
/// call goes.
///
/// The budget covers SSH connection setup, the MCP handshake, the discovery
/// call, and — on a routed session — `tools/list`, because a caller waiting on
/// the list cannot tell those phases apart and a per-phase budget would
/// multiply the worst case by the number of phases.
///
/// It deliberately does not cover the routed `tools/call` itself. That request
/// is stamped with its own [`DEFAULT_ROUTED_DELIVERY_TIMEOUT`] budget once
/// classification is done, so the round trips spent choosing a destination
/// never shorten the tool's execution time [ORB-11023].
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a routed `tools/call` gets, measured from the moment its request
/// is written rather than from session start.
///
/// The mux advertises the whole canonical tool surface, including long-running
/// mutating tools (`orbit.command.exec`, `orbit.workflow.ship`), so a delivery
/// budget sized like a handshake would make those tools unusable over the mux.
/// This is the ceiling on one remote tool, not on the session: exceeding it is
/// reported as [`OrbitError::OutcomeUnknown`], because the request was already
/// on the wire and may have committed.
pub const DEFAULT_ROUTED_DELIVERY_TIMEOUT: Duration = Duration::from_secs(900);

/// The MCP protocol revision this client negotiates. Pinned to the revision
/// Orbit's own server answers with, so a probe fails loudly on a real protocol
/// change rather than silently degrading.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// What one destination reported for this call.
#[derive(Debug, Clone)]
pub struct DestinationSnapshot {
    /// The `machine_id` the destination put on its own v1 envelope. The mux
    /// compares this against the operator's config pin.
    pub machine_id: String,
    pub workspaces: Vec<Workspace>,
}

/// One destination's live answer.
///
/// A trait rather than a concrete SSH call so the mux's projection, ordering,
/// routing, and failure handling are testable against fake destinations
/// without a reachable host.
pub trait DestinationProbe: Send + Sync {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError>;

    /// One short-lived session for a single routed `tools/call`.
    ///
    /// List and route never share a session or a health cache: a list that
    /// showed a workspace does not decide the next call's error.
    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError>;
}

/// The MCP conversation opened for one routed call.
///
/// Snapshot, advertised tools, and the tool call share the session so mixed
/// version and health checks do not pay a second SSH handshake. The mux
/// classifies live errors *before* `call_tool`; a stale or unreachable
/// destination must not observe the call.
pub trait RoutedSession: Send {
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError>;
    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError>;
    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError>;
}

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
    ) -> Self {
        Self {
            caller_machine_id,
            probe_timeout,
            delivery_timeout,
            orchestrator,
        }
    }
}

impl DestinationProbe for SshDestinationProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        let mut session = self.start_session(destination)?;
        session.discover_workspaces()
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        Ok(Box::new(SshRoutedSession::new(
            self.start_session(destination)?,
            self.delivery_timeout,
        )))
    }
}

/// In-process probe for the accepting machine: list and route through the same
/// local [`crate::McpHost`] the v1 MCP surface uses, so local selectors never
/// spawn SSH.
pub struct InProcessDestinationProbe {
    inner: Arc<dyn crate::McpHost>,
    session_context: ToolSessionContext,
}

impl InProcessDestinationProbe {
    pub fn new(inner: Arc<dyn crate::McpHost>, session_context: ToolSessionContext) -> Self {
        Self {
            inner,
            session_context,
        }
    }
}

impl DestinationProbe for InProcessDestinationProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        let content = self.inner.call_tool(
            crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL,
            json!({}),
            self.session_context.clone(),
        )?;
        snapshot_from_discovery_content(destination, &content)
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let snapshot = self.probe(destination)?;
        Ok(Box::new(InProcessRoutedSession {
            inner: Arc::clone(&self.inner),
            session_context: self.session_context.clone(),
            snapshot,
        }))
    }
}

struct InProcessRoutedSession {
    inner: Arc<dyn crate::McpHost>,
    /// Trusted server-owned fields captured before initialize. Per-call audit
    /// evidence is overlaid at dispatch and never written back here.
    session_context: ToolSessionContext,
    snapshot: DestinationSnapshot,
}

impl RoutedSession for InProcessRoutedSession {
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        Ok(self.snapshot.clone())
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        Ok(self
            .inner
            .list_mcp_tool_definitions()?
            .into_iter()
            .map(|definition| mcp_advertised_tool_name(&definition.schema.name))
            .collect())
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        call_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut destination_context = self.session_context.clone();
        destination_context.trace_id = call_context.trace_id;
        destination_context.self_reported_actor = call_context.self_reported_actor;
        self.inner.call_tool(name, arguments, destination_context)
    }
}

/// Dispatch local destinations to an in-process probe and remotes to SSH.
pub struct CompositeDestinationProbe {
    local: Arc<dyn DestinationProbe>,
    remote: Arc<dyn DestinationProbe>,
}

impl CompositeDestinationProbe {
    pub fn new(local: Arc<dyn DestinationProbe>, remote: Arc<dyn DestinationProbe>) -> Self {
        Self { local, remote }
    }

    fn probe_for(&self, destination: &Destination) -> &dyn DestinationProbe {
        if destination.is_local() {
            &*self.local
        } else {
            &*self.remote
        }
    }
}

impl DestinationProbe for CompositeDestinationProbe {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        self.probe_for(destination).probe(destination)
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        self.probe_for(destination).open_route(destination)
    }
}

impl SshDestinationProbe {
    fn start_session(&self, destination: &Destination) -> Result<DestinationSession, OrbitError> {
        let child = spawn_destination_session(
            destination,
            &self.caller_machine_id,
            self.orchestrator.as_deref(),
        )?;
        // The session is one process; the guard ends it on every path,
        // including the timeout path where the child is still mid-answer.
        let mut session =
            DestinationSession::start(destination.clone(), child, self.probe_timeout)?;
        session.handshake()?;
        Ok(session)
    }
}

/// Production routed session: one SSH child, several MCP requests, then drop.
pub(super) struct SshRoutedSession {
    session: DestinationSession,
    snapshot: Option<DestinationSnapshot>,
    tools: Option<Vec<String>>,
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
            delivery_timeout,
        }
    }
}

impl RoutedSession for SshRoutedSession {
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        if let Some(snapshot) = &self.snapshot {
            return Ok(snapshot.clone());
        }
        let snapshot = self.session.discover_workspaces()?;
        self.snapshot = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        if let Some(tools) = &self.tools {
            return Ok(tools.clone());
        }
        let tools = self.session.list_tools()?;
        self.tools = Some(tools.clone());
        Ok(tools)
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        _session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
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
) -> Result<Child, OrbitError> {
    let ssh = destination.ssh_target().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "local destination '{}' cannot be opened over SSH",
            destination.machine_id
        ))
    })?;
    Command::new("ssh")
        .arg("-T")
        .arg("--")
        .arg(ssh)
        .arg(crate::remote::remote_serve_command(
            caller_machine_id,
            orchestrator,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The destination's logs are its own; folding them into this process's
        // stderr would interleave many hosts' output with no attribution.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unreachable(destination, format!("could not start SSH: {error}")))
}

/// How to name a request whose answer never arrived, once its bytes are on
/// the wire.
///
/// Losing the answer is not the same fact for every request. The phases that
/// decide a route are read-only and repeatable, so silence there means the
/// host did not answer. A routed `tools/call` may already have run and
/// committed on the destination, and killing the SSH child does not undo it,
/// so silence there is genuine ambiguity: reporting it as a delivery miss
/// invites the retry that duplicates the write [ORB-11023].
#[derive(Clone, Copy)]
enum LostAnswer<'a> {
    Unreachable,
    OutcomeUnknown { tool: &'a str },
}

impl LostAnswer<'_> {
    fn classify(self, destination: &Destination, request_id: i64, reason: String) -> OrbitError {
        match self {
            Self::Unreachable => unreachable(destination, reason),
            Self::OutcomeUnknown { tool } => OrbitError::OutcomeUnknown {
                // The destination-facing request identity, which is what an
                // operator can correlate against that host's audit log.
                mcp_call_id: format!("{}/{tool}#{request_id}", destination.machine_id),
                message: format!("{reason}; the destination may have completed the call"),
            },
        }
    }
}

/// One MCP client session against a destination, bounded by one deadline at a
/// time.
///
/// The deadline is a budget for the request in flight, not for the session:
/// [`DestinationSession::restart_budget`] re-stamps it when a phase with its
/// own budget begins.
pub(super) struct DestinationSession {
    destination: Destination,
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    deadline: Instant,
    next_id: i64,
}

impl DestinationSession {
    pub(super) fn start(
        destination: Destination,
        mut child: Child,
        timeout: Duration,
    ) -> Result<Self, OrbitError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| unreachable(&destination, "SSH session has no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| unreachable(&destination, "SSH session has no stdout".to_string()))?;
        // A reader thread is what makes the deadline real: a blocking read on
        // an unresponsive host cannot otherwise be abandoned, and the thread
        // ends on its own when the killed child closes the pipe. It is also
        // bounded in both directions: the queue applies backpressure instead
        // of buffering whatever the destination streams while the caller
        // waits, and a line longer than any MCP message ends the session
        // instead of growing a string until this process is killed.
        let (sender, lines) = sync_channel(PROBE_LINE_QUEUE);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader
                    .by_ref()
                    .take(MAX_PROBE_LINE_BYTES)
                    .read_line(&mut line)
                {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if !line.ends_with('\n') && line.len() as u64 >= MAX_PROBE_LINE_BYTES {
                    break;
                }
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            destination,
            child,
            stdin,
            lines,
            deadline: Instant::now() + timeout,
            next_id: 0,
        })
    }

    /// Start a fresh budget for the next phase, discarding whatever the
    /// previous phases left of the old one.
    pub(super) fn restart_budget(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }

    pub(super) fn handshake(&mut self) -> Result<(), OrbitError> {
        let response = self.request_probe(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "orbit-federated-mux",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        let negotiated = response["result"]["protocolVersion"].as_str();
        if negotiated != Some(PROTOCOL_VERSION) {
            return Err(unreachable(
                &self.destination,
                format!("destination negotiated MCP protocol {negotiated:?}"),
            ));
        }
        self.notify("notifications/initialized")
    }

    /// Call the destination's private federated discovery path and return its
    /// envelope. The public v1 list intentionally filters Invalid workspaces.
    pub(super) fn discover_workspaces(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        let response = self.request_probe(
            "tools/call",
            json!({
                "name": crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL,
                "arguments": {},
            }),
        )?;
        let result = &response["result"];
        let content = &result["structuredContent"];
        if result["isError"].as_bool().unwrap_or(false) {
            // The destination's named code survives: wrapping it in a fresh
            // message here would leave the caller matching on prose.
            return Err(remote_tool_error(&self.destination, content));
        }
        snapshot_from_discovery_content(&self.destination, content)
    }

    pub(super) fn list_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        let response = self.request_probe("tools/list", json!({}))?;
        let tools = response["result"]["tools"].as_array().ok_or_else(|| {
            unreachable(
                &self.destination,
                "tools/list answer carried no tools array".to_string(),
            )
        })?;
        Ok(tools
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect())
    }

    /// Deliver one routed tool call.
    ///
    /// Unlike every other request here this one can commit work on the
    /// destination, so a lost answer after the request is written is
    /// [`LostAnswer::OutcomeUnknown`] rather than an unreachable host.
    pub(super) fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, OrbitError> {
        let response = self.request(
            "tools/call",
            json!({
                "name": mcp_advertised_tool_name(name),
                "arguments": arguments,
            }),
            LostAnswer::OutcomeUnknown { tool: name },
        )?;
        let result = &response["result"];
        let content = &result["structuredContent"];
        if result["isError"].as_bool().unwrap_or(false) {
            // Named destination codes such as `capability_refused` must survive
            // as `RemoteTool`, not be wrapped into `execution_failed`.
            return Err(remote_tool_error(&self.destination, content));
        }
        if content.is_null() {
            return Ok(json!({}));
        }
        Ok(content.clone())
    }

    /// A request whose loss tells the caller nothing was delivered.
    fn request_probe(&mut self, method: &str, params: Value) -> Result<Value, OrbitError> {
        self.request(method, params, LostAnswer::Unreachable)
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        lost: LostAnswer<'_>,
    ) -> Result<Value, OrbitError> {
        self.next_id += 1;
        let id = self.next_id;
        // A failed write is pre-dispatch by construction: the destination
        // never saw the request, so it stays an unreachable host even for a
        // delivery.
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        self.await_response(method, id, lost)
    }

    fn notify(&mut self, method: &str) -> Result<(), OrbitError> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }))
    }

    fn send(&mut self, message: &Value) -> Result<(), OrbitError> {
        let mut line = serde_json::to_string(message).map_err(|error| {
            OrbitError::Execution(format!("serialize federated probe request: {error}"))
        })?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| unreachable(&self.destination, format!("write failed: {error}")))
    }

    /// Read until the response with this id arrives or the deadline passes.
    /// Matching strictly by id keeps a server-initiated message or an
    /// out-of-order answer from being read as this request's result.
    fn await_response(
        &mut self,
        method: &str,
        id: i64,
        lost: LostAnswer<'_>,
    ) -> Result<Value, OrbitError> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("timed out waiting for '{method}'"),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("session ended before answering '{method}'"),
                    ));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(line.trim()) {
                Ok(message) => message,
                Err(error) => {
                    return Err(unreachable(
                        &self.destination,
                        format!("emitted invalid JSON: {error}"),
                    ));
                }
            };
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(unreachable(
                        &self.destination,
                        format!("'{method}' failed: {error}"),
                    ));
                }
                return Ok(message);
            }
        }
    }
}

pub(crate) fn snapshot_from_discovery_content(
    destination: &Destination,
    content: &Value,
) -> Result<DestinationSnapshot, OrbitError> {
    let machine_id = content["machine_id"]
        .as_str()
        .ok_or_else(|| {
            unreachable(
                destination,
                "discovery answer carried no machine_id".to_string(),
            )
        })?
        .to_string();
    let workspaces: Vec<Workspace> = serde_json::from_value(content["workspaces"].clone())
        .map_err(|error| {
            unreachable(
                destination,
                format!("discovery answer was not a workspace list: {error}"),
            )
        })?;
    Ok(DestinationSnapshot {
        machine_id,
        workspaces,
    })
}

impl Drop for DestinationSession {
    fn drop(&mut self) {
        if let Err(error) = self.child.kill() {
            tracing::debug!(
                machine_id = %self.destination.machine_id,
                %error,
                "federated probe session was already gone"
            );
        }
        // Reap it: an unwaited SSH child would linger as a zombie for the life
        // of this long-running server process, once per destination per call.
        let _ = self.child.wait();
    }
}

fn unreachable(destination: &Destination, reason: String) -> OrbitError {
    OrbitError::UnreachableDestination(format!("{}: {reason}", destination.machine_id))
}

/// Preserve a destination's structured tool error as-is.
fn remote_tool_error(destination: &Destination, payload: &Value) -> OrbitError {
    let code = payload["code"]
        .as_str()
        .unwrap_or("execution_failed")
        .to_string();
    let message = payload["message"].as_str().unwrap_or_default();
    OrbitError::RemoteTool {
        code,
        message: format!("{}: {message}", destination.machine_id),
        payload: payload.clone(),
    }
}
