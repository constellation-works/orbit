use std::path::Path;

use clap::{Args, Subcommand, ValueEnum};
use orbit_common::security::child_env::ACTIVITY_MCP_TOOLSETS_ENV;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::{McpSessionAuthority, RemoteProxyArgs};
use orbit_types::tool::{McpRole, McpToolset, McpToolsetSelection};

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
    /// New entries register agent-only authority, as bare `orbit mcp serve`
    /// does. Existing operator entries retain their launch and workspace
    /// binding; remove the entry first to replace it with agent authority.
    /// `--federated` instead adds a separate client entry for the
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
    /// is. The socket accepts a connection only when the peer's user id is the
    /// server's. A different user is refused before any MCP message is read. It
    /// binds loopback unless a wider bind is asked for explicitly. A remote TCP
    /// peer has no local user id and is refused; reach the listener through an
    /// SSH tunnel as this user. HTTP requests, including those sent by browser
    /// pages, are closed before their bodies can be dispatched as MCP messages.
    ///
    /// `--workspace <selector>` binds every accepted session to that workspace
    /// by default, as `orbit mcp serve --workspace` does; a client that
    /// announces a workspace, or passes one on a call, still takes precedence.
    /// Without it each session names its own.
    Listen(ListenArgs),
}

impl Execute for McpSubcommand {
    fn execute(self, _runtime: &OrbitRuntime) -> CommandOut {
        match self {
            // All MCP subcommands are dispatched runtime-free by the operation
            // registry (`RuntimeNeed::Forbidden`, `dispatch_mcp`) before runtime
            // initialization. They reach this path only if invoked indirectly
            // (currently never), so use the same runtime-less call chain for safety.
            Self::Init(args) => args.execute_without_runtime(None),
            Self::Remove(args) => args.execute_without_runtime(None),
            Self::Serve(args) => args.execute_without_runtime(None),
            Self::Listen(args) => args.execute_without_runtime(None, None),
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
    /// host registered with `orbit host add`.
    Federated,
}

/// Contract detail behind the `orbit mcp serve` flags lives in the installed
/// Orbit skill; help keeps the safety rule and a pointer to it.
const SERVE_CONTRACT_HELP: &str = "\
Omit `--operator` for any server an agent launches: without it a session holds agent \
capability only.\n\
Mode, authority, workspace binding, attribution, role and toolset rules: installed Orbit \
skill `references/tool-surface.md` (Serving MCP).";

#[derive(Args)]
#[command(
    about = "Serve the Orbit tool registry over Model Context Protocol",
    after_help = SERVE_CONTRACT_HELP
)]
pub struct ServeArgs {
    /// Run as a client to other Orbit servers instead of serving this machine
    #[arg(long, value_name = "MODE")]
    pub mode: Option<ServeMode>,
    /// SSH destination for `--mode remote`: a host, `user@host` or an alias
    #[arg(value_name = "SSH_HOST", requires = "mode")]
    pub ssh_host: Option<String>,
    /// Caller-chosen machine label forwarded by Orbit's SSH routes.
    /// Marks the session as SSH MCP and supplies audit/receipt/claim fences;
    /// it grants no capability and does not prove SSH origination.
    #[arg(long, value_name = "MACHINE_ID", hide = true, conflicts_with = "mode")]
    pub remote_caller_machine_id: Option<String>,
    /// Deterministic owner/follower RPC; selected by Orbit's runtime SSH argv.
    #[arg(
        long,
        hide = true,
        conflicts_with = "mode",
        requires = "remote_caller_machine_id"
    )]
    pub internal_drain: bool,
    /// Host-owned worker projections; selected only by the runtime SSH route.
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["mode", "internal_drain", "operator"],
        requires = "remote_caller_machine_id"
    )]
    pub worker_host: bool,
    /// Serve sessions with operator authority for governed operations
    #[arg(long)]
    pub operator: bool,
    /// Bind sessions to a registered workspace: name, `ws_*` ID or checkout path
    #[arg(long, value_name = "SELECTOR", conflicts_with = "mode")]
    pub workspace: Option<String>,
    /// Attribute tasks this session creates to a crew (attribution only)
    #[arg(long, value_name = "CREW")]
    pub orchestrator: Option<String>,
    /// Serve one role's toolsets: operator, manager, resident, agent or reader
    #[arg(long, value_name = "ROLE")]
    pub role: Option<String>,
    /// Also advertise these comma-separated toolsets (`all` for every toolset)
    #[arg(long, value_name = "TOOLSETS")]
    pub toolset: Option<String>,
}

impl ServeArgs {
    pub fn execute_without_runtime(self, root_override: Option<&Path>) -> CommandOut {
        if matches!(self.mode, Some(ServeMode::Remote))
            && (root_override.is_some()
                || std::env::var("ORBIT_ROOT").is_ok_and(|value| !value.trim().is_empty()))
        {
            return Err(OrbitError::InvalidInput(
                "orbit mcp serve --mode remote cannot apply a local Orbit data root override (--root or ORBIT_ROOT); configure the Orbit root on the SSH destination instead"
                    .to_string(),
            ));
        }
        let global_root = orbit_core::runtime::resolve_generation_root(root_override)?;
        let worker = orbit_core::OrbitRuntime::current_worker_invocation(&global_root)?;
        if worker.is_some()
            && (self.operator
                || self.internal_drain
                || self.worker_host
                || matches!(self.mode, Some(ServeMode::Remote)))
        {
            return Err(OrbitError::PolicyDenied(
                "managed workers require an agent session with preserved invocation context".into(),
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
                    toolsets: resolve_mcp_toolsets(
                        &global_root,
                        self.role.as_deref(),
                        self.toolset.as_deref(),
                    )?,
                })?
            }
            Some(ServeMode::Federated) => {
                if let Some(ssh_host) = self.ssh_host {
                    return Err(OrbitError::InvalidInput(format!(
                        "`orbit mcp serve --mode federated` takes no SSH destination, but got \
                         '{ssh_host}'; register remote hosts with `orbit host add <ssh-target>`"
                    )));
                }
                let toolsets = resolve_mcp_toolsets(
                    &global_root,
                    self.role.as_deref(),
                    self.toolset.as_deref(),
                )?;
                super::server::serve_mcp_federated_stdio(
                    global_root,
                    self.orchestrator,
                    requested_authority(self.operator),
                    toolsets,
                )?
            }
            None => {
                // An Orbit SSH route (the mux, a worker or drain route) names
                // no role or toolset: its caller filters, so this end serves
                // them all.
                let toolsets = if self.toolset.is_none()
                    && self.role.is_none()
                    && self.remote_caller_machine_id.is_some()
                {
                    McpToolsetSelection::all()
                } else {
                    resolve_mcp_toolsets(
                        &global_root,
                        self.role.as_deref(),
                        self.toolset.as_deref(),
                    )?
                };
                super::server::serve_mcp_stdio(
                    global_root,
                    self.remote_caller_machine_id,
                    requested_authority(self.operator),
                    self.workspace
                        .or_else(orbit_core::runtime::managed_workspace_selector_from_env),
                    self.orchestrator,
                    super::server::StdioLaunch {
                        internal_drain: self.internal_drain,
                        worker_host: self.worker_host,
                    },
                    toolsets,
                )?
            }
        }
        Ok(CommandOutput::Silent)
    }
}

/// The toolsets a server advertises: the role (`--role`, else the
/// `machine.mcp.role` host config) or the default surface without one, plus
/// the named toolsets (`--toolset`, else `machine.mcp.toolsets`), plus any a
/// managed run's dispatch stamped in `ORBIT_ACTIVITY_MCP_TOOLSETS`.
///
/// An unreadable host config or stamp falls back without it, with a warning,
/// rather than refusing to serve: a role or toolset trims context, it grants
/// nothing. A malformed flag or config value is refused.
pub(super) fn resolve_mcp_toolsets(
    global_root: &Path,
    role_flag: Option<&str>,
    toolset_flag: Option<&str>,
) -> Result<McpToolsetSelection, OrbitError> {
    let selection = configured_mcp_toolsets(global_root, role_flag, toolset_flag)?;
    let Some(stamped) = std::env::var(ACTIVITY_MCP_TOOLSETS_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(selection);
    };
    match selection.clone().with_names(stamped.split(',')) {
        Ok(stamped) => Ok(stamped),
        Err(error) => {
            tracing::warn!(
                target: "orbit.mcp",
                %error,
                "ignoring {ACTIVITY_MCP_TOOLSETS_ENV}"
            );
            Ok(selection)
        }
    }
}

/// Warn once per retired `--toolset` entry, which [`McpToolsetSelection`]
/// ignores so existing launch scripts keep serving the live surface.
fn warn_retired_flagged_toolsets(names: &[&str]) {
    for name in names {
        if McpToolset::is_retired_toolset_name(name) {
            tracing::warn!(
                target: "orbit.mcp",
                value = name.trim(),
                note = McpToolset::RETIRED_AGENT_INVOKE_NOTE,
                "--toolset entry 'agent-invoke' was removed and is ignored; remove it from the invocation",
            );
        }
    }
}

fn configured_mcp_toolsets(
    global_root: &Path,
    role_flag: Option<&str>,
    toolset_flag: Option<&str>,
) -> Result<McpToolsetSelection, OrbitError> {
    let flagged_role = role_flag
        .map(|role| {
            role.parse::<McpRole>()
                .map_err(|error| OrbitError::InvalidInput(format!("--role: {error}")))
        })
        .transpose()?;
    let flagged_toolsets = toolset_flag.map(|list| list.split(',').collect::<Vec<_>>());
    let config = if flagged_role.is_some() && flagged_toolsets.is_some() {
        None
    } else {
        match orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(
            global_root,
        )) {
            Ok(config) => Some(config.snapshot),
            Err(error) => {
                tracing::warn!(
                    target: "orbit.mcp",
                    %error,
                    "could not read {} or {}; serving without them",
                    orbit_mcp::MCP_ROLE_CONFIG_KEY,
                    orbit_mcp::MCP_TOOLSETS_CONFIG_KEY
                );
                None
            }
        }
    };
    let role = match flagged_role {
        Some(role) => Some(role),
        None => config
            .as_ref()
            .and_then(|config| config.machine_mcp_role.as_deref())
            .map(|role| {
                role.parse::<McpRole>().map_err(|error| {
                    OrbitError::InvalidInput(format!("{}: {error}", orbit_mcp::MCP_ROLE_CONFIG_KEY))
                })
            })
            .transpose()?,
    };
    let base = role.map_or_else(
        McpToolsetSelection::default_only,
        McpToolsetSelection::for_role,
    );
    match flagged_toolsets {
        Some(names) => {
            warn_retired_flagged_toolsets(&names);
            base.with_names(names)
                .map_err(|error| OrbitError::InvalidInput(format!("--toolset: {error}")))
        }
        None => base
            .with_names(
                config
                    .iter()
                    .flat_map(|config| config.machine_mcp_toolsets.iter().map(String::as_str)),
            )
            .map_err(|error| {
                OrbitError::InvalidInput(format!("{}: {error}", orbit_mcp::MCP_TOOLSETS_CONFIG_KEY))
            }),
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
