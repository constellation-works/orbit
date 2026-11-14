//! SSH destination probes and routed MCP sessions.
//!
//! A probe for a listing always opens its own session and closes it after the
//! answer. A routed call checks a session out of the probe's
//! [`RoutedSessionPool`], or opens one, and parks it again after an intact
//! answer; see [`super::pool`] for what that reuse may and may not carry.

use std::process::{Child, Stdio};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use serde_json::{Value, json};

use super::super::config::Destination;
use super::contracts::{DestinationProbe, DestinationSnapshot, RoutedSession};
use super::discovery::unreachable;
use super::local::crews_arguments;
use super::pool::{RoutedSessionKey, RoutedSessionPool};
use super::session::DestinationSession;
use crate::remote::McpSessionAuthority;

/// The production probe: SSH-hosted MCP sessions, fresh per listing and
/// pooled per destination for routed calls.
pub struct SshDestinationProbe {
    #[cfg(feature = "test-support")]
    fixture_line_cap: Option<u64>,
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
    /// Routed sessions parked between calls. Private to this probe unless a
    /// longer-lived owner shares one through [`Self::with_session_pool`].
    pool: RoutedSessionPool,
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
            #[cfg(feature = "test-support")]
            fixture_line_cap: None,
            caller_machine_id,
            probe_timeout,
            delivery_timeout,
            orchestrator,
            authority,
            pool: RoutedSessionPool::default(),
        }
    }

    /// Park routed sessions in `pool` instead of a pool of this probe's own,
    /// so a host that builds a probe per call still reuses sessions across
    /// calls.
    pub fn with_session_pool(mut self, pool: RoutedSessionPool) -> Self {
        self.pool = pool;
        self
    }

    /// Lower this probe's line caps for isolated transport fixtures only.
    ///
    /// Available only with `test-support`, enabled from dev-dependencies.
    /// No process-wide setting or production limit is changed.
    #[cfg(feature = "test-support")]
    pub fn with_test_line_cap(mut self, cap: u64) -> Self {
        self.fixture_line_cap = Some(cap);
        self
    }

    fn session_from_child(
        &self,
        destination: &Destination,
        child: Child,
    ) -> Result<DestinationSession, OrbitError> {
        #[cfg(feature = "test-support")]
        if let Some(cap) = self.fixture_line_cap {
            return DestinationSession::start_with_test_line_cap(
                destination.clone(),
                child,
                self.probe_timeout,
                cap,
            );
        }
        DestinationSession::start(destination.clone(), child, self.probe_timeout)
    }
}

impl DestinationProbe for SshDestinationProbe {
    fn open_internal_drain_route(
        &self,
        destination: &Destination,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        Ok(Box::new(self.open_pooled_route(
            destination,
            SessionShape {
                binding: None,
                authority: McpSessionAuthority::Agent,
                internal: true,
                host_call: false,
            },
        )?))
    }
    fn open_worker_route(
        &self,
        destination: &Destination,
        context: &ToolSessionContext,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let binding = context.worker_invocation.as_ref();
        Ok(Box::new(self.open_pooled_route(
            destination,
            SessionShape {
                binding,
                authority: self.session_authority(binding),
                internal: false,
                host_call: context.worker_host_call,
            },
        )?))
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
        Ok(Box::new(self.open_pooled_route(
            destination,
            SessionShape {
                binding: None,
                authority: self.authority,
                internal: false,
                host_call: false,
            },
        )?))
    }
}

/// What a session asks of its destination: the argv flags `ssh` passes and
/// the worker binding its handshake sends.
#[derive(Clone, Copy)]
struct SessionShape<'a> {
    binding: Option<&'a orbit_types::tool::WorkerInvocation>,
    authority: McpSessionAuthority,
    internal: bool,
    host_call: bool,
}

impl SshDestinationProbe {
    /// A listing's session: always fresh, never pooled. Parked routed
    /// sessions for the destination close first, so the listing's fresh
    /// runtime is never the one waiting on a parked runtime's lock.
    fn start_session(&self, destination: &Destination) -> Result<DestinationSession, OrbitError> {
        let shape = SessionShape {
            binding: None,
            authority: self.authority,
            internal: false,
            host_call: false,
        };
        self.pool
            .close_idle_for_destination(&self.pool_key(destination, shape));
        self.start_shaped_session(destination, shape)
    }

    /// A worker-bound session asks only for agent authority.
    fn session_authority(
        &self,
        binding: Option<&orbit_types::tool::WorkerInvocation>,
    ) -> McpSessionAuthority {
        if binding.is_some() {
            McpSessionAuthority::Agent
        } else {
            self.authority
        }
    }

    fn start_shaped_session(
        &self,
        destination: &Destination,
        shape: SessionShape<'_>,
    ) -> Result<DestinationSession, OrbitError> {
        let child = spawn_destination_session(
            destination,
            &self.caller_machine_id,
            self.orchestrator.as_deref(),
            shape.authority,
            shape.internal,
            shape.host_call,
        )?;
        // The session is one process; the guard ends it on every path,
        // including the timeout path where the child is still mid-answer.
        let mut session = self.session_from_child(destination, child)?;
        session.handshake_with_worker(shape.binding)?;
        Ok(session)
    }

    fn pool_key(&self, destination: &Destination, shape: SessionShape<'_>) -> RoutedSessionKey {
        #[cfg(feature = "test-support")]
        let line_cap = self.fixture_line_cap;
        #[cfg(not(feature = "test-support"))]
        let line_cap = None;
        RoutedSessionKey {
            machine_id: destination.machine_id.clone(),
            ssh_target: destination.ssh_target().unwrap_or_default().to_string(),
            caller_machine_id: self.caller_machine_id.clone(),
            orchestrator: self.orchestrator.clone(),
            authority: shape.authority,
            internal: shape.internal,
            worker_host: shape.host_call,
            worker_invocation: shape.binding.cloned(),
            line_cap,
        }
    }

    /// The session for one routed call: a parked one for the same key when
    /// it provably still answers, else a fresh one.
    ///
    /// A parked session must pass its reuse health check (alive, nothing
    /// unread, an exact bounded `ping`) and then runs this call's discovery
    /// at once, on a freshly stamped probe budget. Any failure before the
    /// routed `tools/call` is written discards it and opens a fresh session
    /// instead of failing the call: `ping` and discovery are read-only, so
    /// nothing was dispatched and starting over cannot duplicate work. The
    /// earlier pool returned a live-but-broken parked session's failure as
    /// the call's answer, and a desynchronised or lock-blocked session cost
    /// the call its whole delivery budget.
    ///
    /// Before a fresh session spawns, sessions parked for the same
    /// destination under any other key are closed: an idle parked runtime
    /// must never hold the destination's runtime lock against the fresh
    /// one.
    fn open_pooled_route(
        &self,
        destination: &Destination,
        shape: SessionShape<'_>,
    ) -> Result<SshRoutedSession, OrbitError> {
        let key = self.pool_key(destination, shape);
        if let Some(mut session) = self.pool.checkout(&key) {
            if session.health_check_for_reuse(self.probe_timeout) {
                session.rearm_for_reuse(self.probe_timeout);
                match session.discover_workspaces(json!({})) {
                    Ok(snapshot) => {
                        let mut routed = SshRoutedSession::new(session, self.delivery_timeout)
                            .returning_to(self.pool.clone(), key);
                        routed.snapshot = Some(snapshot);
                        return Ok(routed);
                    }
                    Err(error) => {
                        tracing::debug!(
                            machine_id = %destination.machine_id,
                            %error,
                            "parked federated session failed discovery; opening a fresh one"
                        );
                    }
                }
            }
            // Discard it: dropping kills the child and releases whatever
            // the destination runtime held.
            drop(session);
        }
        self.pool.close_idle_for_destination(&key);
        let session = self.start_shaped_session(destination, shape)?;
        Ok(SshRoutedSession::new(session, self.delivery_timeout)
            .returning_to(self.pool.clone(), key))
    }
}

/// Production routed session: one SSH child carrying one call's requests.
///
/// The snapshot and tool surface it learns belong to this call only. When
/// the call's answer arrived intact, dropping it parks the SSH child in its
/// pool for the next call; any other end closes the child.
pub(super) struct SshRoutedSession {
    /// `None` only once dropped.
    session: Option<DestinationSession>,
    snapshot: Option<DestinationSnapshot>,
    tools: Option<Vec<String>>,
    tool_schemas: std::collections::HashMap<String, Value>,
    /// The budget the tool call itself gets, stamped at dispatch rather than
    /// at session start.
    delivery_timeout: Duration,
    /// Where the session goes back after an intact answer, if anywhere.
    pool: Option<(RoutedSessionPool, RoutedSessionKey)>,
    /// Set once a dispatched call got an answer whose outcome is known. A
    /// session refused before dispatch, or whose answer was lost, is never
    /// parked [ORB-11023].
    answered_dispatch: bool,
}

impl SshRoutedSession {
    pub(super) fn new(session: DestinationSession, delivery_timeout: Duration) -> Self {
        Self {
            session: Some(session),
            snapshot: None,
            tools: None,
            tool_schemas: std::collections::HashMap::new(),
            delivery_timeout,
            pool: None,
            answered_dispatch: false,
        }
    }

    pub(super) fn returning_to(mut self, pool: RoutedSessionPool, key: RoutedSessionKey) -> Self {
        self.pool = Some((pool, key));
        self
    }

    fn live(&mut self) -> Result<&mut DestinationSession, OrbitError> {
        self.session
            .as_mut()
            .ok_or_else(|| OrbitError::Execution("routed session already closed".into()))
    }

    /// Record a dispatched call's outcome: only an intact answer leaves the
    /// session fit to park. That is a success, or a `RemoteTool` answer —
    /// the destination's structured refusal, JSON-RPC refusals included —
    /// because both prove the whole response arrived framed. A lost,
    /// timed-out or otherwise unanswered call never parks [ORB-11023].
    fn settle_dispatch(&mut self, outcome: Result<Value, OrbitError>) -> Result<Value, OrbitError> {
        self.answered_dispatch = matches!(outcome, Ok(_) | Err(OrbitError::RemoteTool { .. }));
        outcome
    }
}

impl Drop for SshRoutedSession {
    fn drop(&mut self) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        if let Some((pool, key)) = self.pool.take()
            && self.answered_dispatch
            && session.reusable()
        {
            pool.park(key, session);
        }
    }
}

impl RoutedSession for SshRoutedSession {
    fn internal_drain_protocol(&mut self) -> Result<bool, OrbitError> {
        let response = self
            .live()?
            .request_probe(crate::internal_drain::PREFLIGHT_METHOD, json!({}))?;
        Ok(response["result"]["protocol"].as_u64() == Some(crate::INTERNAL_DRAIN_PROTOCOL))
    }

    fn call_internal_drain(
        &mut self,
        name: &str,
        arguments: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let delivery_timeout = self.delivery_timeout;
        let session = self.live()?;
        if context.worker_invocation.is_some() || session.worker_invocation.is_some() {
            return Err(crate::internal_drain::refusal());
        }
        session.restart_budget(delivery_timeout);
        let outcome = session.call_internal_drain(name, arguments);
        self.settle_dispatch(outcome)
    }
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        if let Some(snapshot) = &self.snapshot {
            return Ok(snapshot.clone());
        }
        let snapshot = self.live()?.discover_workspaces(json!({}))?;
        self.snapshot = Some(snapshot.clone());
        Ok(snapshot)
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        if let Some(tools) = &self.tools {
            return Ok(tools.clone());
        }
        let definitions = self.live()?.list_tool_definitions()?;
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
        let delivery_timeout = self.delivery_timeout;
        let session = self.live()?;
        if session_context.worker_invocation != session.worker_invocation {
            return Err(OrbitError::PolicyDenied(
                "SSH worker binding mismatch".into(),
            ));
        }
        // Classification is finished, so the tool's own budget starts here:
        // the SSH setup, handshake, discovery, and `tools/list` round trips
        // that chose this destination must not shorten it.
        session.restart_budget(delivery_timeout);
        let outcome = session.call_tool(name, arguments);
        self.settle_dispatch(outcome)
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
    crate::remote::ssh_command(ssh)?
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
