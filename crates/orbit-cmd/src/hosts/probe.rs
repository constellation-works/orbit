//! Live identity reads: this host in-process, a remote over the federated
//! probe session.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_mcp::federated::{
    DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, Destination, DestinationProbe,
    SshDestinationProbe,
};
use orbit_mcp::{HostFacts, McpSessionAuthority};
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::workspace::Workspace;

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
    }
}

pub(super) fn local_binary_version() -> &'static str {
    orbit_core::application::distributed::owner_binary_version()
}

pub(super) fn local_protocol_fingerprint() -> &'static str {
    orbit_store::contracts::distributed_drain_protocol_fingerprint()
}

/// Open the session federated serve uses (`ssh -T -- <target> orbit mcp
/// serve --remote-caller-machine-id <local id>`) within the federated probe
/// budget and read the host's discovery envelope. Read-only, with ordinary
/// agent authority: it never asks for `--operator`.
///
/// `label` names the host in transport errors; before registration that is
/// the SSH target itself.
pub(super) fn probe_ssh(
    ssh: &str,
    label: &str,
    caller_machine_id: &str,
) -> Result<LiveHost, OrbitError> {
    let probe = SshDestinationProbe::new(
        caller_machine_id.to_string(),
        DEFAULT_PROBE_TIMEOUT,
        DEFAULT_ROUTED_DELIVERY_TIMEOUT,
        None,
        McpSessionAuthority::Agent,
    );
    let snapshot = probe.probe(&Destination::ssh(ssh, label))?;
    Ok(LiveHost {
        facts: snapshot.host,
        workspaces: snapshot.workspaces,
    })
}

/// Run `probe` over every item at once, each within its own budget, so one
/// unreachable host never holds the others. Results keep the input order.
pub(super) fn in_parallel<T, R>(items: &[T], probe: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    std::thread::scope(|scope| {
        let handles = items
            .iter()
            .map(|item| {
                let probe = &probe;
                scope.spawn(move || probe(item))
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| match handle.join() {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

/// The stable class of a probe failure, for the row's `error.code`.
pub(super) fn error_class(error: &OrbitError) -> String {
    match error {
        OrbitError::UnreachableDestination(_) | OrbitError::OutcomeUnknown { .. } => {
            "unreachable_destination".to_string()
        }
        OrbitError::HostRegistry { code, .. } => code.as_str().to_string(),
        OrbitError::RemoteTool { code, .. } => code.clone(),
        _ => "probe_failed".to_string(),
    }
}
