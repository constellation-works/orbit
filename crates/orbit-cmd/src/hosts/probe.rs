//! Live identity reads: this host in-process, a remote over the federated
//! probe session.

use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_core::app::host::{local_binary_version, local_protocol_fingerprint};
use orbit_mcp::federated::{
    DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, Destination, DestinationProbe,
    SshDestinationProbe,
};
use orbit_mcp::{HostFacts, McpSessionAuthority};
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::workspace::Workspace;

// Doctor reports a snapshot, so it does not need the longer routing budget.
const DOCTOR_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// What a remote host said about itself on one probe.
pub(super) struct LiveHost {
    pub(super) facts: HostFacts,
    pub(super) workspaces: Vec<Workspace>,
}

/// This host's own facts, as its discovery envelope reports them.
///
/// `machine_id` is the identity the serving process already resolved, so a
/// machine without `[machine]` keeps the fallback label it has always sent.
pub fn local_host_facts(global_root: &Path, machine_id: &str) -> HostFacts {
    let identity = match inspect_machine_identity(global_root) {
        Ok(MachineIdentityState::Present(identity)) => Some(identity),
        _ => None,
    };
    HostFacts {
        machine_id: machine_id.to_string(),
        machine_name: identity.as_ref().map(|identity| identity.name.clone()),
        task_prefix: identity.map(|identity| identity.task_prefix),
        binary_version: Some(local_binary_version().to_string()),
        protocol_fingerprint: Some(local_protocol_fingerprint().to_string()),
        host_os: orbit_types::task::HostOs::current(),
    }
}

/// Open the session federated serve uses (`ssh -T -- <target> orbit mcp
/// serve --remote-caller-machine-id <local id>`) within the federated probe
/// budget and read the host's discovery envelope. Read-only, with ordinary
/// agent authority: it never asks for `--operator`.
///
/// Transport errors name the host by its SSH target, which an operator can
/// check with `ssh` itself, and carry the tail of ssh's stderr.
pub(super) fn probe_ssh(ssh: &str, caller_machine_id: &str) -> Result<LiveHost, OrbitError> {
    probe_ssh_with_timeout(ssh, caller_machine_id, DEFAULT_PROBE_TIMEOUT)
}

pub(super) fn doctor_probe_ssh(ssh: &str, caller_machine_id: &str) -> Result<LiveHost, OrbitError> {
    probe_ssh_with_timeout(ssh, caller_machine_id, DOCTOR_PROBE_TIMEOUT)
}

fn probe_ssh_with_timeout(
    ssh: &str,
    caller_machine_id: &str,
    timeout: Duration,
) -> Result<LiveHost, OrbitError> {
    let probe = SshDestinationProbe::new(
        caller_machine_id.to_string(),
        timeout,
        DEFAULT_ROUTED_DELIVERY_TIMEOUT,
        None,
        McpSessionAuthority::Agent,
    );
    let snapshot = probe.probe(&Destination::ssh(ssh, ssh))?;
    Ok(LiveHost {
        facts: snapshot.host,
        workspaces: snapshot.workspaces,
    })
}

/// A probe failure's detail for the row's `error.message`: the error without
/// the class `error.code` already carries.
pub(super) fn error_detail(error: &OrbitError) -> String {
    match error {
        OrbitError::UnreachableDestination(message)
        | OrbitError::HostRegistry { message, .. }
        | OrbitError::RemoteTool { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

/// The stable class of a probe failure, for the row's `error.code`.
pub(super) fn error_class(error: &OrbitError) -> String {
    match error {
        OrbitError::UnreachableDestination(_) | OrbitError::OutcomeUnknown { .. } => {
            "unreachable_destination".to_string()
        }
        OrbitError::HostRegistry { code, .. } => code.as_str().to_string(),
        OrbitError::RemoteTool { code, .. } => code.to_string(),
        _ => "probe_failed".to_string(),
    }
}
