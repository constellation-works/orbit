//! Byte-faithful stdio proxy from a local MCP client to a remote Orbit server.
//!
//! The proxy owns no Orbit execution policy. It starts one non-interactive SSH
//! process for the MCP session and lets that process inherit stdin, stdout, and
//! stderr. Workspace selection, checkout resolution, and tool discovery
//! therefore happen only in the remote server.
//!
//! What the proxy *does* own is the authority the destination is asked to
//! serve, because it is the side that composes the remote argv. See
//! [`remote_serve_command`].

use std::path::Path;
use std::process::{Command, Stdio};

use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::process::shell::quote_posix_arg;
use orbit_registry::{MachineIdentityState, inspect_machine_identity};

use super::identity::McpSessionAuthority;

/// Audit-only identity used when this machine has no persisted Orbit identity.
/// Pinned mcp-bridge conformance-v1 audit label; see
/// [`super::identity`]. Not renamed by ORB-12725.
pub(super) const LOCAL_CALLER_MACHINE_ID_FALLBACK: &str = "host/local";

/// What `orbit mcp serve --mode remote` was asked to do.
#[derive(Debug, Clone)]
pub struct RemoteProxyArgs {
    /// SSH destination accepted by `ssh`, such as a host, `user@host`, or a
    /// configured alias.
    pub ssh_host: String,
    /// Orchestrator attribution default to configure on the remote server
    /// [ORB-11313]. Forwarded in the remote argv because the proxy is
    /// byte-faithful and never edits the JSON-RPC stream.
    pub orchestrator: Option<String>,
    /// Authority this client was started with, and therefore the authority the
    /// destination is asked to serve [ORB-12564].
    pub authority: McpSessionAuthority,
}

/// Relay this process's MCP stdio directly through one non-PTY SSH child.
pub fn serve_mcp_remote_proxy(args: RemoteProxyArgs) -> Result<(), OrbitError> {
    let caller_machine_id = local_caller_machine_id();
    let mut command = ssh_command(&args, &caller_machine_id);

    tracing::info!(
        ssh_host = %args.ssh_host,
        caller_machine_id = %caller_machine_id,
        "starting direct SSH MCP proxy"
    );

    let status = command.status().map_err(|error| {
        OrbitError::Execution(format!(
            "could not start SSH MCP proxy to '{}': {error}",
            args.ssh_host
        ))
    })?;
    if status.success() {
        return Ok(());
    }

    Err(OrbitError::Execution(format!(
        "SSH MCP proxy to '{}' exited with status {status}",
        args.ssh_host
    )))
}

/// Build the exact child process used by the proxy.
///
/// `-T` is load-bearing: allocating a PTY can echo input and transform line or
/// control bytes, corrupting the MCP JSON-RPC stream. Explicit inherited stdio
/// keeps this process out of the protocol entirely.
pub(super) fn ssh_command(args: &RemoteProxyArgs, caller_machine_id: &str) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-T")
        .arg("--")
        .arg(&args.ssh_host)
        .arg(remote_serve_command(
            caller_machine_id,
            args.orchestrator.as_deref(),
            args.authority,
        ))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command
}

/// Remote command whose hidden argument marks an SSH-originated MCP session.
///
/// `authority` is the operator statement, and it travels [ORB-12564]. Orbit is
/// a single-user tool and an SSH login to a destination is ownership of it:
/// anyone who can run `ssh box "orbit mcp serve --operator"` can equally run
/// `ssh box "ORBIT_OPERATOR=1 orbit tool run …"`, so asking the far side for a
/// second authorization statement bought nothing and cost every destination a
/// per-caller setup. A client the operator started with `--operator` therefore
/// composes an operator argv for each destination; one started without it
/// composes an agent argv.
///
/// The guard that matters is on this side, and it is here: a client running
/// inside a managed run or otherwise declaring itself an agent never emits
/// `--operator`, whatever the process it was launched from held. Orbit governs
/// agents, not people.
///
/// `orchestrator` is the caller's attribution default for the session it is
/// opening [ORB-11313]. Unlike the authority it grants nothing.
pub(crate) fn remote_serve_command(
    caller_machine_id: &str,
    orchestrator: Option<&str>,
    authority: McpSessionAuthority,
) -> String {
    let mut command = "orbit mcp serve".to_string();
    if propagated_authority(authority) == McpSessionAuthority::Operator {
        command.push_str(" --operator");
    }
    command.push_str(" --remote-caller-machine-id ");
    command.push_str(&quote_posix_arg(caller_machine_id));
    if let Some(orchestrator) = orchestrator
        .map(str::trim)
        .filter(|orchestrator| !orchestrator.is_empty())
    {
        command.push_str(" --orchestrator ");
        command.push_str(&quote_posix_arg(orchestrator));
    }
    command
}

/// The authority this client may actually ask a destination for.
///
/// One chokepoint for both client paths — the v1 proxy and the federated mux —
/// so neither can forget the agent downgrade.
fn propagated_authority(requested: McpSessionAuthority) -> McpSessionAuthority {
    if requested == McpSessionAuthority::Operator && agent_context_declared() {
        tracing::warn!(
            target: "orbit.mcp.remote",
            "this MCP client runs as an agent, so its operator authority is not propagated to \
             the destination; the remote session will hold `agent`"
        );
        return McpSessionAuthority::Agent;
    }
    requested
}

/// Resolve the caller's persisted machine identity, or the audit-only fallback.
///
/// Identity is metadata, not a credential, so an absent or unreadable local
/// identity must not prevent a client from reaching the authoritative server.
pub(super) fn caller_machine_id_at(global_root: Option<&Path>) -> String {
    let state = global_root.map(inspect_machine_identity);
    match state {
        Some(Ok(MachineIdentityState::Present(identity))) => identity.id,
        Some(Err(error)) => {
            tracing::warn!(%error, "could not read local Orbit machine identity; using audit fallback");
            LOCAL_CALLER_MACHINE_ID_FALLBACK.to_string()
        }
        Some(Ok(MachineIdentityState::Absent)) | None => {
            LOCAL_CALLER_MACHINE_ID_FALLBACK.to_string()
        }
    }
}

fn local_caller_machine_id() -> String {
    let global_root = orbit_common::fs::path::global_orbit_dir().ok();
    caller_machine_id_at(global_root.as_deref())
}
