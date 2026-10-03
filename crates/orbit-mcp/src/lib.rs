#![deny(clippy::print_stderr, clippy::print_stdout)]
// Internal MCP kernel surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Orbit's Model Context Protocol framing, tool surface, and transports.
//!
//! This crate owns protocol framing, advertised-name translation, structured
//! responses, canonical tool discovery, server identity presentation, the TCP
//! listener transport, the direct SSH stdio proxy, and the federated mux over
//! operator-configured destinations. Workspace resolution, domain validation,
//! auditing, and authorization remain behind the injected [`McpHost`]
//! boundary.

mod adapter;
mod error;
#[doc(hidden)]
pub mod federated;
mod internal_drain;
mod listener;
mod remote;
mod stdio_session;

use std::collections::BTreeSet;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

pub use adapter::OrbitToolServer;
pub use internal_drain::{INTERNAL_DRAIN_PROTOCOL, internal_drain_name};
pub use listener::{DEFAULT_MCP_LISTEN_PORT, ListenerExposure, McpListener};
pub use remote::{
    FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL, McpServerIdentity, McpSessionAuthority,
    RemoteProxyArgs, canonical_mcp_tool_definitions, execute_discovery_tool,
    execute_federated_workspace_discovery, ignored_caller_authorization_paths, mcp_server_identity,
    safe_mcp_tool_names, serve_mcp_remote_proxy, warn_ignored_caller_authorization,
};
pub use stdio_session::{RESUME_ENV, StdioExit};

/// Back-end for the complete MCP tool surface.
///
/// The host returns the definitions it intends to expose and receives every
/// canonicalized call with one trusted per-call context. The host owns domain
/// authorization and cross-machine routing.
pub trait McpHost: Send + Sync + 'static {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError>;

    /// Return the bound workspace's friction taxonomy for tools/list schema
    /// decoration. An unbound or routing-only host returns `None`, which makes
    /// the schema advertise the shipped defaults.
    fn friction_tag_taxonomy(
        &self,
        _session_context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, OrbitError> {
        Ok(None)
    }

    /// Canonical names to leave out of this session's `tools/list`: tools the
    /// host serves but that are switched off wherever this session's calls
    /// would land (a plugin disabled in the bound workspace). Advisory and
    /// read on every `tools/list`, so a toggle change shows without a new
    /// session; `tools/call` enforces the same state on its own.
    fn hidden_tool_names(&self, _session_context: &ToolSessionContext) -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError>;

    /// Execute a deterministic protocol operation through a launch-selected
    /// internal transport. The default host has no such route.
    fn call_internal_drain(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(internal_drain::refusal())
    }

    /// Persist a public/internal-route refusal using the host's audit boundary.
    fn refuse_internal_drain(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(internal_drain::refusal())
    }

    /// Whether workspace-scoped tools on this host take the federated
    /// host-qualified selector rather than a v1 local selector.
    ///
    /// The default is the authoritative server: a registered name, a logical
    /// `ws_*`, or an absolute path. The federated mux overrides this so
    /// `tools/list` tells callers to copy `selector` from federated
    /// `orbit.workspace.list` and routing refuses anything else as
    /// `unknown_selector`.
    fn federated_workspace_selectors(&self) -> bool {
        false
    }
}

/// Serve MCP stdio with trusted session context.
///
/// Resumes a session handed over by a previous image of this process, and
/// returns [`StdioExit::HandOver`] when this one should hand over in turn;
/// see [`stdio_session`].
pub async fn serve_stdio_with_context(
    host: Arc<dyn McpHost>,
    trusted_context: ToolSessionContext,
) -> Result<StdioExit, OrbitError> {
    let server = OrbitToolServer::new_with_context(host, trusted_context);
    stdio_session::serve(server, stdio_session::resumed_session()).await
}

/// Serve the deterministic drain RPC on a server explicitly launched for it.
/// Client names, metadata and public tools/call never enable this route.
pub async fn serve_internal_drain_stdio(
    host: Arc<dyn McpHost>,
    trusted_context: ToolSessionContext,
) -> Result<StdioExit, OrbitError> {
    let mut server = OrbitToolServer::new_with_context(host, trusted_context);
    server.internal_drain = true;
    stdio_session::serve(server, stdio_session::resumed_session()).await
}
