//! `orbit mcp` — MCP client integration and server.
//!
//! `orbit mcp init/remove` manages local client integration for Claude Code,
//! Codex, Gemini, and Grok. `orbit mcp serve` serves the Orbit tool surface over
//! MCP so external clients can discover and invoke Orbit operations with typed
//! JSON schemas, and `orbit mcp listen` serves that same surface on a TCP
//! socket for deployments that need one.

mod command;
mod listen;
mod server;
mod setup;

pub use command::{McpCommand, McpSubcommand};
pub(crate) use orbit_mcp::safe_mcp_tool_names;
pub(crate) use server::ID_RESOLVED_WORKSPACE_TOOLS;
#[allow(unused_imports)]
pub(crate) use setup::init_auto_for_workspace;

pub(crate) const ORBIT_MCP_SERVER_ID: &str = "orbit";
