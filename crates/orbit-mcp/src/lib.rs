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
mod listener;
mod remote;

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use serde_json::Value;

pub use adapter::OrbitToolServer;
pub use listener::{DEFAULT_MCP_LISTEN_PORT, ListenerExposure, McpListener};
pub use remote::{
    FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL, McpServerIdentity, McpSessionAuthority,
    RemoteProxyArgs, canonical_mcp_tool_definitions, execute_discovery_tool,
    execute_federated_workspace_discovery, ignored_caller_authorization_paths, mcp_server_identity,
    safe_mcp_tool_names, serve_mcp_remote_proxy, warn_ignored_caller_authorization,
};

/// Back-end for the complete MCP tool surface.
///
/// The host returns the definitions it intends to expose and receives every
/// canonicalized call with one trusted per-call context. The kernel performs no
/// authorization or cross-machine routing.
pub trait McpHost: Send + Sync + 'static {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError>;

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError>;

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

/// Serve the given host over MCP stdio with a default local context.
pub async fn serve_stdio(host: Arc<dyn McpHost>) -> Result<(), OrbitError> {
    serve_stdio_with_context(host, ToolSessionContext::trusted_local(None, None, None)).await
}

/// Serve MCP stdio with trusted session context.
pub async fn serve_stdio_with_context(
    host: Arc<dyn McpHost>,
    trusted_context: ToolSessionContext,
) -> Result<(), OrbitError> {
    let server = OrbitToolServer::new_with_context(host, trusted_context);
    serve_server(server).await
}

async fn serve_server(server: OrbitToolServer) -> Result<(), OrbitError> {
    let running = server
        .serve(stdio())
        .await
        .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio start: {error}")))?;
    running
        .waiting()
        .await
        .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?;
    Ok(())
}
