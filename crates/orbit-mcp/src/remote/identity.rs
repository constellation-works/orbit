//! Server-local identity facts attached to one MCP stdio session.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::tool::{McpCapability, McpTransport, ToolSessionContext};

use orbit_registry::{MachineIdentityState, inspect_machine_identity, os_hostname};

/// Pinned mcp-bridge conformance-v1 audit label. Deliberately keeps the
/// `host/` spelling through the ORB-12725 rename: it is a wire value other
/// implementations match on, not Orbit's own vocabulary.
const LOCAL_MACHINE_FALLBACK: &str = "host/local";
const LOCAL_MACHINE_NAME_FALLBACK: &str = "local";

/// The authority an MCP server process serves its sessions with.
///
/// This is the only place an MCP session acquires capabilities: the tool
/// chokepoint resolves an MCP call from the session alone, so a server that
/// stamps nothing here can never reach a governed operation. The choice is made
/// once, when the server process starts, and never from the live process
/// environment — an agent that launches its own `orbit mcp serve` gets
/// [`Self::Agent`] no matter what the launching shell exported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpSessionAuthority {
    /// The ordinary agent surface.
    #[default]
    Agent,
    /// A server an operator deliberately started for themselves. Sessions also
    /// keep `agent`, because an operator may do everything an agent may.
    Operator,
}

impl McpSessionAuthority {
    pub(crate) fn capabilities(self) -> BTreeSet<McpCapability> {
        match self {
            Self::Agent => BTreeSet::from([McpCapability::Agent]),
            Self::Operator => BTreeSet::from([McpCapability::Agent, McpCapability::Operator]),
        }
    }
}

/// Identity and trusted audit context derived by the accepting machine.
pub struct McpServerIdentity {
    pub process_machine_id: String,
    pub process_machine_name: String,
    pub session_context: ToolSessionContext,
}

/// Resolve the accepting machine and build its trusted session envelope.
///
/// `remote_caller_machine_id` is an opaque label forwarded by the SSH proxy or
/// the federated mux. It marks the session as SSH-originated for audit and
/// names the calling machine; it is not an authenticated principal and grants
/// nothing. `SSH_CONNECTION`, when present, contributes only the best-effort
/// caller IP.
///
/// `authority` is what this server process was started to serve, and a session
/// that arrived over SSH is stamped with it exactly as a local one is: the
/// argv came from a caller who holds an SSH login to this machine, which is
/// already the strongest statement about it anyone can make [ORB-12564].
pub fn mcp_server_identity(
    global_root: &Path,
    remote_caller_machine_id: Option<String>,
    authority: McpSessionAuthority,
) -> Result<McpServerIdentity, OrbitError> {
    let (process_machine_id, process_machine_name) = local_identity(global_root)?;
    // A forwarded label is the whole origination test. The destination no
    // longer second-guesses it from `SSH_CONNECTION` or terminal shape: those
    // observations only ever decided how much of the argv to honor, and the
    // argv is now honored in full.
    let is_remote = remote_caller_machine_id.is_some();
    let caller_machine_id = remote_caller_machine_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| process_machine_id.clone());
    let session_context = ToolSessionContext {
        caller_machine_id: Some(caller_machine_id),
        caller_machine_name: (!is_remote).then(|| process_machine_name.clone()),
        process_machine_id: Some(process_machine_id.clone()),
        process_machine_name: Some(process_machine_name.clone()),
        transport: Some(if is_remote {
            McpTransport::SshMcp
        } else {
            McpTransport::Local
        }),
        caller_ip: is_remote.then(observed_ssh_caller_ip).flatten(),
        effective_capabilities: authority.capabilities(),
        ..ToolSessionContext::default()
    };
    Ok(McpServerIdentity {
        process_machine_id,
        process_machine_name,
        session_context,
    })
}

pub(super) fn local_identity(global_root: &Path) -> Result<(String, String), OrbitError> {
    match inspect_machine_identity(global_root)? {
        MachineIdentityState::Present(identity) => Ok((identity.id, identity.name)),
        MachineIdentityState::Absent => Ok((
            LOCAL_MACHINE_FALLBACK.to_string(),
            os_hostname().unwrap_or_else(|| LOCAL_MACHINE_NAME_FALLBACK.to_string()),
        )),
    }
}

fn observed_ssh_caller_ip() -> Option<String> {
    std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|connection| ssh_caller_ip(&connection))
}

pub(super) fn ssh_caller_ip(connection: &str) -> Option<String> {
    connection
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned)
        .filter(|value| !value.is_empty())
}
