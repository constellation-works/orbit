//! Host routing: task ids to the host their prefix names, and `--host` to the
//! selector that host lists [ORB-14449].
//!
//! The prefix table and the routes are both the host file. Delivery is the
//! federated client the pull drains already use for their owner, opened with
//! the caller's own authority and never more.

use std::path::Path;
use std::sync::Arc;

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_mcp::McpSessionAuthority;
use orbit_mcp::federated::{
    self, DEFAULT_PROBE_TIMEOUT, DEFAULT_ROUTED_DELIVERY_TIMEOUT, FederatedMcpHost,
    SshDestinationProbe,
};
use orbit_registry::hosts::{
    HostEntry, HostRegistry, ResolvedHost, TaskPrefixRoute, load_host_registry,
};
use orbit_registry::workspace_registry::{load_registry_from_read_only, registry_path_for};

use super::require_local;

/// Where a call that addresses one task by id goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskIdRoute {
    /// This host's prefix, or an id with no parseable prefix: run in-process.
    Local,
    /// A registered remote host writes this id.
    Remote(HostEntry),
}

/// Route `task_id` by its prefix, or refuse a prefix no host claims.
pub fn route_task_id(global_root: &Path, task_id: &str) -> Result<TaskIdRoute, OrbitError> {
    let registry = load_host_registry(global_root)?;
    route_in(&registry, task_id)
}

fn route_in(registry: &HostRegistry, task_id: &str) -> Result<TaskIdRoute, OrbitError> {
    let table = registry.task_prefix_table();
    match table.route(task_id) {
        TaskPrefixRoute::Local => Ok(TaskIdRoute::Local),
        TaskPrefixRoute::Host(entry) => Ok(TaskIdRoute::Remote(entry)),
        TaskPrefixRoute::Unregistered { prefix } => {
            Err(orbit_registry::hosts::unknown_task_prefix(task_id, &prefix))
        }
    }
}

/// The v1 MCP refusal for an id-only call whose prefix is another host's.
/// v1 servers do not relay, so the caller is told where the task lives.
pub fn task_prefix_remote(task_id: &str, holder: &HostEntry) -> OrbitError {
    OrbitError::host_registry(
        HostRegistryCode::TaskPrefixRemote,
        format!(
            "task {task_id} is held by host '{}' ({}). This MCP server does not relay: use the \
             federated server (`orbit mcp serve --mode federated`), which routes the id there, \
             or pass a `workspace` selector that host lists (`orbit host show {}`)",
            holder.name, holder.machine_id, holder.name
        ),
    )
}

/// The registered remote host that holds `task_id`, if one does. An id whose
/// prefix is this host's, unparseable or unregistered is not remote.
pub fn remote_task_holder(
    global_root: &Path,
    task_id: &str,
) -> Result<Option<HostEntry>, OrbitError> {
    Ok(match route_task_id(global_root, task_id) {
        Ok(TaskIdRoute::Remote(entry)) => Some(entry),
        Ok(TaskIdRoute::Local) => None,
        Err(error) if error.host_registry_code() == Some(HostRegistryCode::UnknownTaskPrefix) => {
            None
        }
        Err(error) => return Err(error),
    })
}

/// The federated client a CLI call or a replica GC lookup routes through.
///
/// Only remote destinations are delivered over it: a local id runs
/// in-process before this is ever composed. `authority` is the caller's own;
/// the SSH argv downgrades an operator request from an agent process again.
pub fn routed_client(
    global_root: &Path,
    caller_machine_id: &str,
    authority: McpSessionAuthority,
) -> Result<FederatedMcpHost, OrbitError> {
    let registry = load_host_registry(global_root)?;
    let destinations = federated::federated_membership(
        caller_machine_id,
        caller_machine_id,
        registry.hosts().routes(),
    );
    let probe = SshDestinationProbe::new(
        caller_machine_id.to_string(),
        DEFAULT_PROBE_TIMEOUT,
        DEFAULT_ROUTED_DELIVERY_TIMEOUT,
        None,
        authority,
    );
    let root = global_root.to_path_buf();
    Ok(FederatedMcpHost::new(destinations, Arc::new(probe))
        .with_task_prefix_routing(registry.task_prefix_table())
        .with_local_mirror_hint(Arc::new(move |task_id| {
            local_mirror_workspace(&root, task_id)
        })))
}

/// The local workspace whose store holds a copy of `task_id`, by name.
pub fn local_mirror_workspace(global_root: &Path, task_id: &str) -> Option<String> {
    crate::task_owner::resolve_task_owner(global_root, task_id)
        .ok()
        .map(|selected| selected.workspace.name)
}

/// What `--host <host>` with a workspace value resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostWorkspaceRoute {
    /// This host: the workspace's own `ws_*` id, resolved locally.
    Local { workspace_id: String },
    /// A registered host: the selector it lists, and the entry.
    Remote { selector: String, holder: HostEntry },
}

/// Resolve `--host <host> --workspace <workspace>` (or `--pull`).
///
/// The host resolves like `orbit host show`. A remote host's live list is
/// read and the matching descriptor's `selector` copied; it is never built by
/// concatenation. A legacy row has no name or prefix and is refused: register
/// it with `orbit host add` first.
pub fn resolve_host_workspace(
    global_root: &Path,
    host: &str,
    workspace: &str,
) -> Result<HostWorkspaceRoute, OrbitError> {
    let registry = load_host_registry(global_root)?;
    let local = require_local(&registry)?;
    let entry = match registry.resolve(host)? {
        ResolvedHost::Local(_) => {
            return local_workspace(global_root, &local.name, workspace);
        }
        ResolvedHost::Entry(entry) => entry.clone(),
        ResolvedHost::Legacy(row) => {
            return Err(OrbitError::host_registry(
                HostRegistryCode::UnknownHost,
                format!(
                    "'{}' is a legacy destination row with no host name; run `orbit host add {}` \
                     to register it before naming it with --host",
                    row.ssh, row.ssh
                ),
            ));
        }
    };
    // Discovery only: the probe that reads the list asks for no authority.
    let client = routed_client(global_root, &local.id, McpSessionAuthority::Agent)?;
    let selector = client.host_workspace_selector(&entry.machine_id, workspace)?;
    Ok(HostWorkspaceRoute::Remote {
        selector,
        holder: entry,
    })
}

/// `--host` naming this machine: the workspace must be one it registers.
fn local_workspace(
    global_root: &Path,
    machine_name: &str,
    workspace: &str,
) -> Result<HostWorkspaceRoute, OrbitError> {
    let registry = load_registry_from_read_only(&registry_path_for(global_root))?.registry;
    let wanted = workspace.trim();
    let matches = registry
        .workspaces
        .iter()
        .filter(|candidate| candidate.id == wanted || candidate.name == wanted)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(HostWorkspaceRoute::Local {
            workspace_id: only.id.clone(),
        }),
        [] => {
            let listed = registry
                .workspaces
                .iter()
                .map(|workspace| format!("{} ({})", workspace.name, workspace.id))
                .collect::<Vec<_>>();
            Err(OrbitError::StaleRoute(format!(
                "host '{machine_name}' does not list workspace '{wanted}'; it lists: {}",
                if listed.is_empty() {
                    "no workspaces".to_string()
                } else {
                    listed.join(", ")
                }
            )))
        }
        _ => Err(OrbitError::UnknownSelector(format!(
            "'{wanted}' names more than one workspace on host '{machine_name}'; pass the `ws_*` \
             id instead"
        ))),
    }
}

/// The SSH target to run a host-local command on, for the hint a command
/// that rejects `--host` gives. `None` names this machine.
pub fn host_ssh_target(global_root: &Path, host: &str) -> Result<Option<String>, OrbitError> {
    let registry = load_host_registry(global_root)?;
    Ok(match registry.resolve(host)? {
        ResolvedHost::Local(_) => None,
        ResolvedHost::Entry(entry) => Some(entry.ssh.clone()),
        ResolvedHost::Legacy(row) => Some(row.ssh.clone()),
    })
}

/// The registered host a host-qualified selector names, when it is a remote
/// host-file entry. A selector for this machine, or for no entry, is `None`.
pub fn selector_remote_host(
    global_root: &Path,
    selector: &str,
) -> Result<Option<HostEntry>, OrbitError> {
    let Ok(parsed) = selector.parse::<federated::MachineQualifiedSelector>() else {
        return Ok(None);
    };
    let registry = load_host_registry(global_root)?;
    if registry
        .local()
        .is_some_and(|local| local.id == parsed.machine_id())
    {
        return Ok(None);
    }
    Ok(registry
        .hosts()
        .entries()
        .iter()
        .find(|entry| entry.machine_id == parsed.machine_id())
        .cloned())
}
