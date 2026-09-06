#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Args, Subcommand, ValueEnum};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::{McpSessionAuthority, RemoteProxyArgs, SshAcceptance};

use crate::command::{CommandOut, CommandOutput, Execute};

use super::callers::CallersArgs;
use super::listen::ListenArgs;
use super::setup::{InitArgs, RemoveArgs};

/// True only when Linux reported that `execve` itself entered a protected
/// credential-transition state and Orbit reinforced it before CLI parsing.
/// A public flag cannot set this bit.
static SSH_ACCEPTANCE_LAUNCH_VERIFIED: AtomicBool = AtomicBool::new(false);
static SSH_ACCEPTANCE_LOGIN_SHELL_VERIFIED: AtomicBool = AtomicBool::new(false);
static SSH_ACCEPTANCE_TOKEN: OnceLock<String> = OnceLock::new();

/// Verify the kernel boundary around an sshd-provided acceptance bearer.
///
/// The generated Tier 2 account must use a setgid Orbit copy, whose group
/// differs from the login account's real group, as its login shell. Linux
/// applies its secure-exec dumpability policy as part of that `execve`, before
/// the dynamic loader or Rust startup can expose the initial environment
/// through `/proc`. This first Rust operation verifies the inherited state,
/// permanently drops the otherwise privilege-free launch group, and selects
/// the strict non-dumpable value as defense in depth. It does not claim to
/// protect an ordinary, initially dumpable process retroactively.
pub(crate) fn verify_ssh_acceptance_launch_boundary() {
    #[cfg(target_os = "linux")]
    {
        let Some(token) = std::env::var(orbit_mcp::SSH_ACCEPTANCE_ENV).ok() else {
            return;
        };
        // Safety: these prctl operations read or set one process-local integer
        // attribute and do not dereference any pointers.
        let inherited_dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE) };
        // Safety: credential getters have no pointers or side effects.
        let (same_user, real_group, effective_group) = unsafe {
            (
                libc::getuid() == libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
            )
        };
        let credential_transition = same_user && real_group != effective_group;
        // Linux uses 0 or the administrator-selected suid_dumpable value 2 for
        // a protected credential-changing exec. Value 1 is the ordinary
        // same-UID-readable state reproduced by ORB-11184.
        let protected_at_exec = credential_transition && matches!(inherited_dumpable, 0 | 2);
        // Safety: the real group is one of this process's existing group IDs;
        // setting all three IDs to it permanently discards the setgid launch
        // credential before Orbit opens or creates any task data.
        let launch_group_dropped =
            unsafe { libc::setresgid(real_group, real_group, real_group) } == 0;
        // Safety: see above. Zero is the strict kernel-defined non-dumpable state.
        let reinforced = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } == 0;
        let verified = protected_at_exec && launch_group_dropped && reinforced;
        SSH_ACCEPTANCE_LAUNCH_VERIFIED.store(verified, Ordering::Release);
        if verified {
            let _ = SSH_ACCEPTANCE_TOKEN.set(token);
        }
        // Safety: this is the first operation in single-threaded process
        // startup. Removing the inherited value here prevents later command
        // children from receiving the reusable bearer.
        unsafe { std::env::remove_var(orbit_mcp::SSH_ACCEPTANCE_ENV) };
    }
}

/// Adapt sshd's login-shell invocation into Orbit's ordinary CLI argv.
///
/// OpenSSH always invokes a forced command as `<login-shell> -c <command>`.
/// The protected Orbit copy must itself be that login shell, or an ordinary
/// shell would receive the bearer before the setgid exec. Only the exact shape
/// emitted by `callers authorize` is adapted; every other `-c` invocation is
/// left for clap to refuse.
pub(crate) fn normalize_ssh_login_shell_args(
    args: impl IntoIterator<Item = OsString>,
) -> Vec<OsString> {
    let args = args.into_iter().collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    {
        if SSH_ACCEPTANCE_TOKEN.get().is_none() || args.len() != 3 || args[1] != OsStr::new("-c") {
            return args;
        }
        let Some(command) = args[2].to_str() else {
            return args;
        };
        let fields = command.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != 7
            || fields[1..4] != ["mcp", "serve", "--accept-ssh"]
            || fields[4] != "--caller"
            || fields[6] != "--operator"
        {
            return args;
        }
        let Ok(current_exe) = std::env::current_exe().and_then(std::fs::canonicalize) else {
            return args;
        };
        let Ok(command_exe) = std::fs::canonicalize(fields[0]) else {
            return args;
        };
        if current_exe != command_exe {
            return args;
        }
        SSH_ACCEPTANCE_LOGIN_SHELL_VERIFIED.store(true, Ordering::Release);
        let mut normalized = Vec::with_capacity(fields.len());
        normalized.push(args[0].clone());
        normalized.extend(fields[1..].iter().map(OsString::from));
        normalized
    }
    #[cfg(not(target_os = "linux"))]
    args
}

/// Read the Tier 2 bearer only after verifying that the kernel hid the initial
/// process metadata before any userspace startup ran.
fn protected_ssh_acceptance_token() -> Result<String, OrbitError> {
    if !SSH_ACCEPTANCE_LAUNCH_VERIFIED.load(Ordering::Acquire)
        || !SSH_ACCEPTANCE_LOGIN_SHELL_VERIFIED.load(Ordering::Acquire)
    {
        return Err(OrbitError::UnauthorizedCaller(
            "SSH MCP acceptance did not enter through the generated Linux setgid login-shell \
             boundary; install a fresh protected launcher as the dedicated account's shell and \
             regenerate the authorized_keys line with `orbit mcp callers authorize --launcher \
             <path>`"
                .to_string(),
        ));
    }
    SSH_ACCEPTANCE_TOKEN.get().cloned().ok_or_else(|| {
        OrbitError::UnauthorizedCaller(
            "SSH MCP acceptance was not supplied by sshd; regenerate and install the \
             authorized_keys line with `orbit mcp callers authorize --launcher <path>`"
                .to_string(),
        )
    })
}

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
    /// Inspect and seed which callers this machine serves, and as what
    ///
    /// On an SSH destination the caller writes the remote argv, so the
    /// authority a remote session asks for is a request. `~/.orbit/mcp-callers.toml`
    /// is this machine's answer, and a remote session holds the intersection
    /// of the two. Local sessions are unaffected.
    Callers(CallersArgs),
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
            Self::Callers(args) => args.execute_without_runtime(None),
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
    /// On a session that arrived over SSH this is a *request*, not a grant.
    /// The machine serving it caps the session at what its
    /// `~/.orbit/mcp-callers.toml` grants the calling machine, so a caller
    /// cannot serve itself operator authority on someone else's host. See
    /// `orbit mcp callers check`.
    #[arg(long, conflicts_with = "mode")]
    pub operator: bool,
    /// Treat this session as SSH-originated after validating the destination
    /// capability carried across the generated Linux setgid login-shell boundary.
    ///
    /// Meaningful only inside the forced command printed by `orbit mcp callers
    /// authorize`. The bearer is deliberately not an argument value: the
    /// generated key entry supplies it through an environment option to a
    /// credential-changing executable that the kernel protects at exec.
    #[arg(long, hide = true, conflicts_with = "mode")]
    pub accept_ssh: bool,
    /// The calling machine's identity, as this machine wrote it beside the
    /// authenticating key.
    ///
    /// Honored only together with `--accept-ssh`, because only there is it
    /// this machine's own statement rather than a string a caller could type.
    /// It selects the `~/.orbit/mcp-callers.toml` row that caps the session,
    /// and unlike a forwarded audit label it is backed by the key sshd checked.
    #[arg(
        long,
        value_name = "MACHINE_ID",
        requires = "accept_ssh",
        conflicts_with = "mode"
    )]
    pub caller: Option<String>,
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
                super::server::serve_mcp_federated_stdio(self.orchestrator)?
            }
            None => super::server::serve_mcp_stdio(
                self.remote_caller_machine_id,
                if self.operator {
                    McpSessionAuthority::Operator
                } else {
                    McpSessionAuthority::Agent
                },
                self.workspace
                    .or_else(orbit_core::runtime::managed_workspace_selector_from_env),
                // `--caller` is unreachable without `--accept-ssh`, so the
                // "honored only under a forced command" rule is carried by the
                // type the server receives rather than re-checked downstream.
                if self.accept_ssh {
                    SshAcceptance::ForcedCommand {
                        caller: self.caller,
                        acceptance_token: protected_ssh_acceptance_token()?,
                    }
                } else {
                    SshAcceptance::Environment
                },
                self.orchestrator,
            )?,
        }
        Ok(CommandOutput::Silent)
    }
}

#[cfg(test)]
#[path = "tests/command.rs"]
mod tests;
