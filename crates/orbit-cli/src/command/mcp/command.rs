use std::path::Path;

use clap::{Args, Subcommand, ValueEnum};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::{McpSessionAuthority, RemoteProxyArgs};

use crate::command::{CommandOut, CommandOutput, Execute};

use super::listen::ListenArgs;
use super::setup::{InitArgs, RemoveArgs};

#[derive(Args)]
#[command(
    about = "Register MCP client integrations and run the MCP server",
    arg_required_else_help = true,
    subcommand_required = true
)]
pub struct McpCommand {
    #[command(subcommand)]
    pub command: McpSubcommand,
}

impl Execute for McpCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum McpSubcommand {
    /// Initialize MCP client integration for the current workspace
    ///
    /// By default, registers agent-only authority — the same as bare `orbit
    /// mcp serve`. `--federated` instead adds a separate client entry for the
    /// session-unbound mux and preserves the default v1 entry. For the
    /// operator-authorized bootstrap integration (workflow dispatch,
    /// `orbit.command.exec`), use `orbit workspace init --mcp` instead.
    Init(InitArgs),
    /// Remove MCP client integration for the current workspace
    Remove(RemoveArgs),
    /// Serve the Orbit tool registry over Model Context Protocol
    Serve(ServeArgs),
    /// Serve the Orbit tool registry over Model Context Protocol on a TCP socket
    ///
    /// This is the transport for deployments that need a socket — a server-side
    /// Orbit reached through an SSH tunnel, for example. `orbit mcp serve`
    /// remains the stdio server that MCP clients launch directly.
    ///
    /// Each accepted connection is an independent MCP session against the same
    /// server-local tool surface, resolved and audited exactly as a stdio session
    /// is. The socket authenticates no client, so it binds loopback unless a
    /// wider bind is asked for explicitly.
    Listen(ListenArgs),
}

impl Execute for McpSubcommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        match self {
            // All MCP subcommands are dispatched runtime-free via main.rs's
            // pattern match before runtime initialization. They reach this
            // path only if invoked indirectly (currently never), so use the
            // same runtime-less call chain for safety.
            Self::Init(args) => args.execute_without_runtime(None),
            Self::Remove(args) => args.execute_without_runtime(None),
            Self::Serve(args) => args.execute_without_runtime(None),
            Self::Listen(args) => args.execute_without_runtime(None),
        }
    }
}

/// Which side of the wire this invocation is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ServeMode {
    /// Present stdio MCP locally and relay it directly to a remote Orbit over
    /// one non-PTY SSH process.
    Remote,
    /// Present one stdio MCP surface over this machine's workspaces plus every
    /// SSH destination configured in `~/.orbit/mcp-destinations.toml`.
    ///
    /// Local workspaces are included automatically and need no destination
    /// row. This mode binds to no single workspace. It lists each destination's
    /// workspaces as live descriptors, probing remotes on every call, and
    /// includes remotes that are unreachable right now rather than hiding
    /// them. Workspace-scoped tools take the host-qualified `selector` copied
    /// from that list; a registered name, a bare `ws_*`, or `--workspace` is
    /// not a federated selector.
    Federated,
}

#[derive(Args)]
#[command(about = "Serve the Orbit tool registry over Model Context Protocol")]
pub struct ServeArgs {
    /// Run as a client to other Orbit servers instead of serving this machine.
    ///
    /// `remote` proxies one chosen SSH destination and requires it as an
    /// argument. `federated` includes this machine automatically and muxes
    /// additional destinations configured in `~/.orbit/mcp-destinations.toml`;
    /// it takes no argument.
    #[arg(long, value_name = "MODE")]
    pub mode: Option<ServeMode>,
    /// SSH destination for `--mode remote`, such as a host, `user@host`, or a
    /// configured alias.
    #[arg(value_name = "SSH_HOST", requires = "mode")]
    pub ssh_host: Option<String>,
    /// Audit identity supplied only by Orbit's direct SSH proxy command.
    /// Presence also marks the server session's transport as SSH MCP.
    #[arg(long, value_name = "MACHINE_ID", hide = true, conflicts_with = "mode")]
    pub remote_caller_machine_id: Option<String>,
    /// Serve sessions with operator authority, so they may perform governed
    /// operations such as dispatching a workflow or deleting a task.
    ///
    /// Omit this for any server an agent launches: without it a session holds
    /// the agent capability only, and governed operations are refused. The flag
    /// is the deliberate act — `ORBIT_OPERATOR` in this process's environment is
    /// ignored on the MCP surface, so an operator shell cannot grant operator
    /// authority to an agent's server by accident.
    ///
    /// With `--mode remote` or `--mode federated` it is also the operator
    /// statement for every SSH destination this client opens: Orbit is a
    /// single-user tool and an SSH login to a machine is ownership of it, so
    /// the destination serves the authority this argv asks for. A client that
    /// is itself running as an agent never propagates it.
    #[arg(long)]
    pub operator: bool,
    /// Bind this server's sessions to a registered workspace: a workspace
    /// name, a logical workspace ID (`ws_*`), or an absolute registered
    /// checkout path.
    ///
    /// Most MCP clients cannot announce `_meta.orbit.workspace` on their
    /// initialize, so without this a workspace-scoped tool must repeat the
    /// selector on every call. Local `orbit mcp init` and `orbit workspace
    /// init --mcp` write this into the integrations they generate; a managed
    /// child that launches `orbit mcp serve` without the flag still inherits
    /// `ORBIT_WORKSPACE` from the trusted execution envelope. The federated
    /// init path is session-unbound. A client that does announce a workspace,
    /// and any explicit per-call `workspace`, still take precedence. The
    /// selector is resolved against the accepting server's registry per call,
    /// never from the server process cwd.
    #[arg(long, value_name = "SELECTOR", conflicts_with = "mode")]
    pub workspace: Option<String>,
    /// Attribute tasks this session creates to a named orchestrator crew,
    /// unless the call names one itself.
    ///
    /// Attribution only. Unlike `--operator` it grants nothing: a session
    /// still holds exactly the capabilities it was served with, and the crew
    /// named here neither selects the crew a task executes under nor the
    /// model recorded for that execution. The name is resolved against the
    /// crews of whichever workspace the call lands in, so an unconfigured
    /// crew fails that call rather than quietly selecting another one.
    ///
    /// It applies only when a task is created, and only to that task: an
    /// existing task's orchestrator is never rewritten from this default.
    ///
    /// Treat it as configuration, not as evidence of who is calling. A
    /// long-lived MCP connection outlives a model switch in the client, so
    /// pass `orchestrator` on the individual call, or restart the connection
    /// with a new value, when the orchestrating crew actually changes.
    #[arg(long, value_name = "CREW")]
    pub orchestrator: Option<String>,
}

impl ServeArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        if root_override.is_some() {
            return Err(OrbitError::InvalidInput(
                "orbit mcp serve does not accept a workspace root override; select a workspace per initialize or tool call"
                    .to_string(),
            ));
        }
        match self.mode {
            Some(ServeMode::Remote) => {
                // Each mode owns its own argument rule, so the pairing lives
                // here rather than in a derive attribute that cannot express
                // "required by one mode and refused by the other".
                let ssh_host = self.ssh_host.ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "`orbit mcp serve --mode remote` needs an SSH destination, e.g. \
                         `orbit mcp serve --mode remote my-box`"
                            .to_string(),
                    )
                })?;
                orbit_mcp::serve_mcp_remote_proxy(RemoteProxyArgs {
                    ssh_host,
                    orchestrator: self.orchestrator,
                    authority: requested_authority(self.operator),
                })?
            }
            Some(ServeMode::Federated) => {
                if let Some(ssh_host) = self.ssh_host {
                    return Err(OrbitError::InvalidInput(format!(
                        "`orbit mcp serve --mode federated` takes no SSH destination, but got \
                         '{ssh_host}'; destinations are configured in \
                         `~/.orbit/mcp-destinations.toml`"
                    )));
                }
                super::server::serve_mcp_federated_stdio(
                    self.orchestrator,
                    requested_authority(self.operator),
                )?
            }
            None => super::server::serve_mcp_stdio(
                self.remote_caller_machine_id,
                requested_authority(self.operator),
                self.workspace
                    .or_else(orbit_core::runtime::managed_workspace_selector_from_env),
                self.orchestrator,
            )?,
        }
        Ok(CommandOutput::Silent)
    }
}

/// The authority a `serve` invocation asks for, local or remote.
fn requested_authority(operator: bool) -> McpSessionAuthority {
    if operator {
        McpSessionAuthority::Operator
    } else {
        McpSessionAuthority::Agent
    }
}

#[cfg(test)]
#[path = "tests/command.rs"]
mod tests;
