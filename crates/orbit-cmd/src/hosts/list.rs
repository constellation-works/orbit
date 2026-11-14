//! `orbit host list` and `show`: this machine's row, every registered remote's
//! row (probed live when asked), and one host's detail with its dependents.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use orbit_common::OrbitError;
use orbit_registry::hosts::{HostRegistry, ResolvedHost, hosts_path, load_host_registry};
use orbit_registry::workspace_registry::{
    WorkspaceRegistryMachineContext, find_workspace_by_id, parse_workspace_registry,
    registry_file_fingerprint, registry_path_for,
};
use orbit_registry::{MachineIdentity, inspect_machine_identity};
use orbit_types::workspace::WorkspaceCheckoutRole;

use super::dependents::host_dependents;
use super::probe::{in_parallel, local_binary_version, local_protocol_fingerprint};
use super::remote::remote_row;
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
    let mut remotes = registry.hosts().entries().iter().collect::<Vec<_>>();
    remotes.sort_by_key(|target| target.name.to_ascii_lowercase());
    hosts.extend(in_parallel(&remotes, |target| {
        remote_row(target, &local.id, probe)
    }));
    Ok(HostList {
        host_file: hosts_path(global_root),
        legacy: false,
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
            host: remote_row(entry, &local.id, true),
            dependents: Some(host_dependents(global_root, &entry.machine_id)?),
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
        host_os: orbit_types::task::HostOs::current().map(|os| os.as_str().to_string()),
        skew: false,
        skew_fields: Vec::new(),
        workspaces: local_workspaces(global_root),
    }
}

// One slot bounds retention even when callers inspect many roots. Only the
// workspace projection is memoized: host facts and remote probe policy remain
// fresh. Read bytes (rather than trusting mtime/length) to observe equal-size
// atomic replacements even on filesystems with coarse timestamp resolution.
struct LocalWorkspaceProjection {
    path: PathBuf,
    content: String,
    context: WorkspaceRegistryMachineContext,
    workspaces: Vec<HostWorkspace>,
}

static LOCAL_WORKSPACE_PROJECTION: Mutex<Option<LocalWorkspaceProjection>> = Mutex::new(None);

fn local_workspaces(global_root: &Path) -> Vec<HostWorkspace> {
    // Serialize sampling and publication too: an older reader must not publish
    // its projection over a newer one and force another parse of that generation.
    let mut cached = LOCAL_WORKSPACE_PROJECTION
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Ok(global_root) = global_root.canonicalize() else {
        *cached = None;
        return Vec::new();
    };
    let path = registry_path_for(&global_root);
    // The loader's path check rejects registry symlinks and inaccessible roots.
    // Never let a cached value bypass that refusal or an unreadable/missing file.
    if registry_file_fingerprint(&path).is_none() {
        *cached = None;
        return Vec::new();
    }
    let Ok(content) = std::fs::read_to_string(&path) else {
        *cached = None;
        return Vec::new();
    };
    let Ok(identity) = inspect_machine_identity(&global_root) else {
        *cached = None;
        return Vec::new();
    };
    let context = WorkspaceRegistryMachineContext::from(&identity);
    if let Some(projection) = cached.as_ref()
        && projection.path == path
        && projection.content == content
        && projection.context == context
    {
        return projection.workspaces.clone();
    }
    tracing::debug!(target: "orbit.hosts.workspace_projection", "workspace_registry_parse");
    let _parse = tracing::debug_span!(target: "orbit.hosts.workspace_projection", "workspace_registry_parse").entered();
    let workspaces = parse_workspace_registry(&content, &context)
        .map(|(registry, _)| project_local_workspaces(&registry))
        .unwrap_or_default();
    // Malformed documents still produce the established empty workspace list;
    // their exact bytes/context are safe to memoize and repairs differ in key.
    *cached = Some(LocalWorkspaceProjection {
        path,
        content,
        context,
        workspaces: workspaces.clone(),
    });
    workspaces
}

fn project_local_workspaces(
    registry: &orbit_types::workspace::WorkspaceRegistry,
) -> Vec<HostWorkspace> {
    registry
        .checkouts
        .iter()
        .filter_map(|checkout| {
            let workspace = find_workspace_by_id(registry, &checkout.workspace_id)?;
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
