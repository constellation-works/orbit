//! Live destination snapshots, probe contracts, and request budgets.

use std::collections::BTreeMap;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workspace::Workspace;
use serde_json::{Map, Value};

use super::super::config::Destination;

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

/// What one destination reported for this call.
#[derive(Debug, Clone, Default)]
pub struct DestinationSnapshot {
    /// The `machine_id` the destination put on its own v1 envelope. The mux
    /// compares this against the operator's config pin.
    pub machine_id: String,
    /// Everything else the destination said about itself on that envelope:
    /// name, task prefix, binary version and pull-protocol fingerprint, each
    /// unknown when the destination predates it.
    pub host: crate::HostFacts,
    pub workspaces: Vec<Workspace>,
    /// Per workspace ID, the crew keys (`crews` or `crews_error`) the
    /// destination attached when asked to include crews. Empty otherwise.
    pub crews: BTreeMap<String, Map<String, Value>>,
}

/// One destination's live answer.
///
/// A trait rather than a concrete SSH call so the mux's projection, ordering,
/// routing, and failure handling are testable against fake destinations
/// without a reachable host.
pub trait DestinationProbe: Send + Sync {
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError>;

    /// [`Self::probe`], asking the destination to attach each workspace's
    /// crews. A destination that predates the request answers without them.
    fn probe_with_crews(
        &self,
        destination: &Destination,
    ) -> Result<DestinationSnapshot, OrbitError> {
        self.probe(destination)
    }

    /// One short-lived session for a single routed `tools/call`.
    ///
    /// List and route never share a session or a health cache: a list that
    /// showed a workspace does not decide the next call's error.
    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError>;

    /// Open only the deterministic protocol endpoint. No public-route fallback.
    fn open_internal_drain_route(
        &self,
        _destination: &Destination,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        Err(crate::internal_drain::refusal())
    }

    /// Refuse a retired public request locally, preserving the accepting audit.
    fn refuse_internal_drain(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(crate::internal_drain::refusal())
    }

    fn open_worker_route(
        &self,
        destination: &Destination,
        context: &ToolSessionContext,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let _ = context;
        self.open_route(destination)
    }
}

/// The MCP conversation opened for one routed call.
///
/// Snapshot, advertised tools, and the tool call share the session so mixed
/// version and health checks do not pay a second SSH handshake. The mux
/// classifies live errors *before* `call_tool`; a stale or unreachable
/// destination must not observe the call.
pub trait RoutedSession: Send {
    fn internal_drain_protocol(&mut self) -> Result<bool, OrbitError> {
        Ok(false)
    }
    fn call_internal_drain(
        &mut self,
        _name: &str,
        _arguments: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(crate::internal_drain::refusal())
    }
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError>;
    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError>;
    /// Verify an extension against the peer's live input schema. Names alone
    /// do not prove an older peer understands newly added arguments.
    fn supports_tool_argument(&mut self, _name: &str, _argument: &str) -> Result<bool, OrbitError> {
        Ok(false)
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError>;
}
