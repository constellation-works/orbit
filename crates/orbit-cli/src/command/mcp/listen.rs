//! `orbit mcp listen` — serve the same MCP host over a TCP socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use clap::Args;
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
    /// Advertise these opt-in toolsets beside the default one, as
    /// `orbit mcp serve --toolset` takes them: `leases`, `experiments`,
    /// `analytics`, `decision-channel`, or `all`. Replaces the
    /// `machine.mcp.toolsets` host config for this listener.
    #[arg(long, value_name = "TOOLSETS")]
    pub toolset: Option<String>,
}

impl ListenArgs {
    pub fn execute_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let global_root = orbit_core::runtime::resolve_generation_root(root_override)?;
        let toolsets = super::command::resolve_mcp_toolsets(&global_root, self.toolset.as_deref())?;
        super::server::serve_mcp_listener(
            global_root,
            self.addr,
            self.exposure(),
            workspace_selector.map(ToOwned::to_owned),
            toolsets,
        )?;
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
