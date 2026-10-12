//! Local composition for the authoritative MCP server process.
//!
//! Both transports — stdio and the TCP listener — serve the same host with the
//! same trusted session envelope, so a call's dispatch and audit path does not
//! depend on how its bytes arrived.

mod dispatch;
mod host;
mod runtime_cache;
mod serve;

pub(crate) use serve::ID_RESOLVED_WORKSPACE_TOOLS;
pub(super) use serve::{serve_mcp_federated_stdio, serve_mcp_listener, serve_mcp_stdio};
