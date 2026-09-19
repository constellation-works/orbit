//! Thin SSH transport and remote-machine MCP support data.

mod discovery;
mod identity;
mod legacy;
mod proxy;
mod surface;

#[cfg(test)]
mod tests;

pub use self::discovery::{
    FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL, execute_discovery_tool,
    execute_federated_workspace_discovery,
};
pub use self::identity::{McpServerIdentity, McpSessionAuthority, mcp_server_identity};
pub use self::legacy::{ignored_caller_authorization_paths, warn_ignored_caller_authorization};
pub use self::proxy::{RemoteProxyArgs, serve_mcp_remote_proxy};
// The federated mux reuses the v1 remote argv verbatim rather than restating
// it, so both client paths present one session shape to a destination.
pub(crate) use self::proxy::remote_serve_command;
pub use self::surface::{canonical_mcp_tool_definitions, safe_mcp_tool_names};
