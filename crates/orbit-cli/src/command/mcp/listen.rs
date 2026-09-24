//! `orbit mcp listen` — serve the same MCP host over a TCP socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use clap::Args;
use orbit_core::OrbitError;
use orbit_mcp::{DEFAULT_MCP_LISTEN_PORT, ListenerExposure};

use crate::command::{CommandOut, CommandOutput};

/// Bind address used when the operator names none.
const DEFAULT_LISTEN_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_MCP_LISTEN_PORT);

/// The long-form command help lives on the `McpSubcommand::Listen` variant,
/// which is where clap reads a subcommand's own description from.
#[derive(Args)]
#[command(about = "Serve the Orbit tool registry over Model Context Protocol on a TCP socket")]
pub struct ListenArgs {
    /// Address to bind, written `IP:PORT`. Defaults to loopback. Local
    /// processes can connect without authentication; browser HTTP requests
    /// are rejected before MCP dispatch.
    #[arg(value_name = "ADDR", default_value_t = DEFAULT_LISTEN_ADDR)]
    pub addr: SocketAddr,
    /// Allow a bind address other hosts can reach. Anyone who reaches the
    /// socket gets this machine's agent-accessible tools, so pass this only
    /// where the network path is already restricted.
    #[arg(long)]
    pub allow_non_loopback: bool,
}

impl ListenArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        if root_override.is_some() {
            return Err(OrbitError::InvalidInput(
                "orbit mcp listen does not accept a workspace root override; select a workspace per initialize or tool call"
                    .to_string(),
            ));
        }
        super::server::serve_mcp_listener(self.addr, self.exposure())?;
        Ok(CommandOutput::Silent)
    }

    fn exposure(&self) -> ListenerExposure {
        if self.allow_non_loopback {
            ListenerExposure::AnyInterface
        } else {
            ListenerExposure::LoopbackOnly
        }
    }
}

#[cfg(test)]
#[path = "tests/listen.rs"]
mod tests;
