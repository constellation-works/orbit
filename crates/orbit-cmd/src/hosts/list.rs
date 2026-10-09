//! `orbit host list` and `show`: this machine's row, every registered remote's
//! row (probed live when asked), and one host's detail with its dependents.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_registry::MachineIdentity;
use orbit_registry::hosts::{
    HostRegistry, RegisteredHosts, ResolvedHost, hosts_path, load_host_registry,
};
use orbit_registry::workspace_registry::{
    find_workspace_by_id, load_registry_from_read_only, registry_path_for,
};
use orbit_types::workspace::WorkspaceCheckoutRole;

use super::dependents::host_dependents;
use super::probe::{in_parallel, local_binary_version, local_protocol_fingerprint};
use super::remote::{RemoteTarget, remote_row, remote_targets};
use super::{HostDetail, HostList, HostRow, HostWorkspace};

/// `orbit host list`.
pub fn list_hosts(global_root: &Path, probe: bool) -> Result<HostList, OrbitError> {
    list_registered_hosts(&load_host_registry(global_root)?, probe)
}

/// [`list_hosts`] over an already loaded host file. The dashboard keeps its
/// last valid load and lists from it while a newer file fails to load.
pub fn list_registered_hosts(registry: &HostRegistry, probe: bool) -> Result<HostList, OrbitError> {
    let global_root = registry.global_root();
    let local = require_local(registry)?;
    let mut hosts = vec![local_row(global_root, local)];
    let mut remotes = remote_targets(registry.hosts());
    remotes.sort_by_key(|target| target.name().to_ascii_lowercase());
    hosts.extend(in_parallel(&remotes, |target| {
        remote_row(target, &local.id, probe)
    }));
    Ok(HostList {
        host_file: hosts_path(global_root),
        legacy: matches!(registry.hosts(), RegisteredHosts::Legacy(_)),
        hosts,
    })
}

/// One host by name or `machine_id`, probed as in [`list_hosts`], with what
/// on this machine depends on it.
pub fn show_host(global_root: &Path, selector: &str) -> Result<HostDetail, OrbitError> {
    show_registered_host(&load_host_registry(global_root)?, selector)
}

/// [`show_host`] over an already loaded host file.
pub fn show_registered_host(
    registry: &HostRegistry,
    selector: &str,
) -> Result<HostDetail, OrbitError> {
    let global_root = registry.global_root();
    let local = require_local(registry)?;
    Ok(match registry.resolve(selector)? {
        ResolvedHost::Local(identity) => HostDetail {
            host: local_row(global_root, identity),
            dependents: None,
        },
        ResolvedHost::Entry(entry) => HostDetail {
            host: remote_row(&RemoteTarget::Entry(entry), &local.id, true),
            dependents: Some(host_dependents(global_root, &entry.machine_id)?),
        },
        ResolvedHost::Legacy(row) => HostDetail {
            host: remote_row(&RemoteTarget::Legacy(row), &local.id, true),
            dependents: Some(host_dependents(global_root, &row.machine_id)?),
        },
    })
}

pub(super) fn require_local(registry: &HostRegistry) -> Result<&MachineIdentity, OrbitError> {
    registry.local().ok_or_else(|| {
        OrbitError::InvalidInput(
            "this machine has no [machine] identity yet; run `orbit init` first".to_string(),
        )
    })
}

fn local_row(global_root: &Path, identity: &MachineIdentity) -> HostRow {
    HostRow {
        name: identity.name.clone(),
        machine_id: identity.id.clone(),
        ssh: None,
        task_prefix: Some(identity.task_prefix.clone()),
        local: true,
        legacy: false,
        reachable: Some(true),
        error: None,
        binary_version: Some(local_binary_version().to_string()),
        protocol_fingerprint: Some(local_protocol_fingerprint().to_string()),
        skew: false,
        skew_fields: Vec::new(),
        workspaces: local_workspaces(global_root),
    }
}

fn local_workspaces(global_root: &Path) -> Vec<HostWorkspace> {
    let Ok(load) = load_registry_from_read_only(&registry_path_for(global_root)) else {
        return Vec::new();
    };
    let registry = load.registry;
    registry
        .checkouts
        .iter()
        .filter_map(|checkout| {
            let workspace = find_workspace_by_id(&registry, &checkout.workspace_id)?;
            let replica = checkout.role == Some(WorkspaceCheckoutRole::Replica);
            Some(HostWorkspace {
                id: workspace.id.clone(),
                name: workspace.name.clone(),
                role: if replica { "replica" } else { "owner" },
                owner_machine_id: if replica {
                    checkout.owner_machine_id.clone()
                } else {
                    workspace.owner_machine_id.clone()
                },
                status: workspace.status.to_string(),
            })
        })
        .collect()
}
