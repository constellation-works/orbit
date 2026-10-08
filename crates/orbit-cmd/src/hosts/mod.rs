//! `orbit host`: the operator's registered remote hosts [ORB-14448].
//!
//! Joins the Registry's host file to the federated MCP probe. Each entry
//! caches only what never changes on a host (`machine_id`, `task_prefix`) plus
//! the operator's name and SSH target; reachability, version, protocol and
//! workspaces are read live from the host whenever a command asks, and never
//! written back. The CLI and the dashboard both call these operations, so
//! validation, migration and writes have one implementation.

mod doctor;
mod mutate;
mod probe;
mod route;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::{HostRegistryCode, OrbitError};
use orbit_registry::MachineIdentity;
use orbit_registry::hosts::{
    HostEntry, HostRegistry, LegacyHost, RegisteredHosts, ResolvedHost, hosts_path,
    load_host_registry,
};
use orbit_registry::workspace_registry::{
    find_workspace_by_id, load_registry_from_read_only, registry_path_for,
};
use orbit_types::workflow::JobRunState;
use orbit_types::workspace::{Workspace, WorkspaceCheckoutRole, WorkspaceRegistry};
use serde::Serialize;

pub use doctor::doctor_hosts_row;
pub use mutate::{HostChange, HostChangeEntry, add_host, remove_host, rename_host};
pub use probe::local_host_facts;
use probe::{
    LiveHost, error_class, error_detail, in_parallel, local_binary_version,
    local_protocol_fingerprint,
};
pub use route::{
    HostWorkspaceRoute, TaskIdRoute, host_ssh_target, local_mirror_workspace, remote_task_holder,
    resolve_host_workspace, route_task_id, routed_client, selector_remote_host, task_prefix_remote,
};

/// One host as `orbit host list` and `show` report it.
#[derive(Debug, Clone, Serialize)]
pub struct HostRow {
    /// The host name. A legacy row has none, so its SSH target stands in.
    pub name: String,
    pub machine_id: String,
    /// `None` for the local host, which is never reached over SSH.
    pub ssh: Option<String>,
    /// Unknown only for a legacy row whose host did not report one.
    pub task_prefix: Option<String>,
    /// This installation, read in-process.
    pub local: bool,
    /// Read from the legacy destinations file, not the host file.
    pub legacy: bool,
    /// Whether the live probe answered; `None` when no probe ran.
    pub reachable: Option<bool>,
    /// Why the live probe produced no facts for this host.
    pub error: Option<HostProbeError>,
    pub binary_version: Option<String>,
    pub protocol_fingerprint: Option<String>,
    /// Set when the host's version or protocol differs from this machine's.
    pub skew: bool,
    /// Which of `binary_version` and `protocol_fingerprint` differ.
    pub skew_fields: Vec<&'static str>,
    pub workspaces: Vec<HostWorkspace>,
}

/// The class and detail of a failed live probe.
#[derive(Debug, Clone, Serialize)]
pub struct HostProbeError {
    pub code: String,
    /// What happened, without `code` repeated in front of it.
    pub message: String,
}

/// One workspace checkout on a host, with its role there.
#[derive(Debug, Clone, Serialize)]
pub struct HostWorkspace {
    pub id: String,
    pub name: String,
    /// `owner`, or `replica` of `owner_machine_id`.
    pub role: &'static str,
    pub owner_machine_id: Option<String>,
    pub status: String,
}

/// `orbit host list`.
#[derive(Debug, Clone, Serialize)]
pub struct HostList {
    pub host_file: PathBuf,
    /// Rows were read from the legacy destinations file.
    pub legacy: bool,
    /// The local host first, then every remote sorted by name.
    pub hosts: Vec<HostRow>,
}

/// `orbit host show`.
#[derive(Debug, Clone, Serialize)]
pub struct HostDetail {
    #[serde(flatten)]
    pub host: HostRow,
    /// What on this machine routes to the host. Absent for the local host.
    pub dependents: Option<HostDependents>,
}

/// Everything on this machine that loses its route if a host goes away.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HostDependents {
    pub replica_checkouts: Vec<ReplicaDependent>,
    pub pull_drains: Vec<PullDrainDependent>,
}

impl HostDependents {
    pub fn is_empty(&self) -> bool {
        self.replica_checkouts.is_empty() && self.pull_drains.is_empty()
    }

    /// One line per dependent, for refusals and human output.
    pub fn describe(&self) -> Vec<String> {
        self.replica_checkouts
            .iter()
            .map(|checkout| {
                format!(
                    "replica checkout {} ({}) at {}",
                    checkout.workspace_name,
                    checkout.workspace_id,
                    checkout.repo_root.display()
                )
            })
            .chain(self.pull_drains.iter().map(|drain| {
                format!(
                    "{} pull drain {} in {}",
                    drain.state, drain.run_id, drain.workspace_id
                )
            }))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplicaDependent {
    pub workspace_id: String,
    pub workspace_name: String,
    pub repo_root: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct PullDrainDependent {
    pub workspace_id: String,
    pub run_id: String,
    pub state: String,
}

/// List this host and every registered one. With `probe`, every remote is
/// probed in parallel; an unreachable host keeps its row with the error.
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

/// What on this machine routes to the host `selector` names: the dependents a
/// `host_in_use` refusal lists. `None` for the local host.
pub fn dependents_of(
    global_root: &Path,
    selector: &str,
) -> Result<Option<HostDependents>, OrbitError> {
    let registry = load_host_registry(global_root)?;
    match registry.resolve(selector)? {
        ResolvedHost::Local(_) => Ok(None),
        resolved => host_dependents(global_root, resolved.machine_id()).map(Some),
    }
}

/// A remote row's cached identity: a host-file entry or a legacy route.
#[derive(Debug, Clone, Copy)]
enum RemoteTarget<'a> {
    Entry(&'a HostEntry),
    Legacy(&'a LegacyHost),
}

impl RemoteTarget<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.name,
            Self::Legacy(row) => &row.ssh,
        }
    }

    fn ssh(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.ssh,
            Self::Legacy(row) => &row.ssh,
        }
    }

    fn machine_id(&self) -> &str {
        match self {
            Self::Entry(entry) => &entry.machine_id,
            Self::Legacy(row) => &row.machine_id,
        }
    }

    fn task_prefix(&self) -> Option<&str> {
        match self {
            Self::Entry(entry) => Some(&entry.task_prefix),
            Self::Legacy(_) => None,
        }
    }
}

fn remote_targets(hosts: &RegisteredHosts) -> Vec<RemoteTarget<'_>> {
    hosts
        .entries()
        .iter()
        .map(RemoteTarget::Entry)
        .chain(hosts.legacy().iter().map(RemoteTarget::Legacy))
        .collect()
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

fn remote_row(target: &RemoteTarget<'_>, caller_machine_id: &str, probe: bool) -> HostRow {
    let mut row = HostRow {
        name: target.name().to_string(),
        machine_id: target.machine_id().to_string(),
        ssh: Some(target.ssh().to_string()),
        task_prefix: target.task_prefix().map(ToOwned::to_owned),
        local: false,
        legacy: matches!(target, RemoteTarget::Legacy(_)),
        reachable: None,
        error: None,
        binary_version: None,
        protocol_fingerprint: None,
        skew: false,
        skew_fields: Vec::new(),
        workspaces: Vec::new(),
    };
    if !probe {
        return row;
    }
    let live = probe::probe_ssh(target.ssh(), caller_machine_id)
        .and_then(|live| verify_identity(target, live));
    match live {
        Ok(live) => {
            row.reachable = Some(true);
            if row.task_prefix.is_none() {
                row.task_prefix = live.facts.task_prefix.clone();
            }
            row.skew_fields = skew_fields(&live.facts);
            row.skew = !row.skew_fields.is_empty();
            row.binary_version = live.facts.binary_version;
            row.protocol_fingerprint = live.facts.protocol_fingerprint;
            row.workspaces = live
                .workspaces
                .iter()
                .map(|workspace| remote_workspace(workspace, target.machine_id()))
                .collect();
        }
        Err(error) => {
            // An identity mismatch did answer; it is listed as reachable, but
            // none of what it said is attributed to this entry.
            row.reachable =
                Some(error.host_registry_code() == Some(HostRegistryCode::HostIdentityMismatch));
            row.error = Some(HostProbeError {
                code: error_class(&error),
                message: error_detail(&error),
            });
        }
    }
    row
}

/// A probe answering with another identity fails closed: the entry is never
/// rewritten from a probe, and a replaced host is removed and added again.
fn verify_identity(target: &RemoteTarget<'_>, live: LiveHost) -> Result<LiveHost, OrbitError> {
    let reported_prefix = live.facts.task_prefix.as_deref();
    let prefix_differs = matches!(
        (target.task_prefix(), reported_prefix),
        (Some(cached), Some(reported)) if cached != reported
    );
    if live.facts.machine_id == target.machine_id() && !prefix_differs {
        return Ok(live);
    }
    Err(OrbitError::host_registry(
        HostRegistryCode::HostIdentityMismatch,
        format!(
            "'{}' answered as machine_id {} with task prefix {}, but the entry records {} with \
             task prefix {}; the entry is unchanged. If the host was reinstalled or replaced, run \
             `orbit host remove {}` and then `orbit host add {}`",
            target.ssh(),
            live.facts.machine_id,
            reported_prefix.unwrap_or("unknown"),
            target.machine_id(),
            target.task_prefix().unwrap_or("unknown"),
            target.name(),
            target.ssh()
        ),
    ))
}

fn skew_fields(facts: &orbit_mcp::HostFacts) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if facts
        .binary_version
        .as_deref()
        .is_some_and(|version| version != local_binary_version())
    {
        fields.push("binary_version");
    }
    if facts
        .protocol_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != local_protocol_fingerprint())
    {
        fields.push("protocol_fingerprint");
    }
    fields
}

/// A remote workspace's role on that host: it owns what its record says it
/// owns (or a pre-identity standalone record), and replicates everything else.
fn remote_workspace(workspace: &Workspace, host_machine_id: &str) -> HostWorkspace {
    let replica = workspace
        .owner_machine_id
        .as_deref()
        .is_some_and(|owner| owner != host_machine_id);
    HostWorkspace {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        role: if replica { "replica" } else { "owner" },
        owner_machine_id: workspace.owner_machine_id.clone(),
        status: workspace.status.to_string(),
    }
}

/// Local replica checkouts whose owner is `machine_id`, and the pull drains
/// pending or running in them. A pull drain always runs in a replica checkout
/// of the owner its selector names, so the checkouts are the complete set to
/// inspect.
pub(super) fn host_dependents(
    global_root: &Path,
    machine_id: &str,
) -> Result<HostDependents, OrbitError> {
    let registry = load_registry_from_read_only(&registry_path_for(global_root))?.registry;
    let mut dependents = HostDependents::default();
    for (workspace, checkout) in replica_checkouts_of(&registry, machine_id) {
        dependents.replica_checkouts.push(ReplicaDependent {
            workspace_id: workspace.id.clone(),
            workspace_name: workspace.name.clone(),
            repo_root: checkout.repo_root.clone(),
        });
        let runtime = match crate::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout_read_only(
            global_root,
            workspace,
            checkout,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, workspace = %workspace.id, "cannot inspect pull drains");
                continue;
            }
        };
        for state in [JobRunState::Running, JobRunState::Pending] {
            let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
                job_id: Some(orbit_core::application::distributed::PULL_DRAIN_JOB.to_string()),
                state: Some(state),
                ..Default::default()
            });
            match runs {
                Ok(runs) => {
                    dependents
                        .pull_drains
                        .extend(runs.into_iter().map(|run| PullDrainDependent {
                            workspace_id: workspace.id.clone(),
                            run_id: run.run_id,
                            state: run.state.to_string(),
                        }))
                }
                Err(error) => {
                    tracing::warn!(%error, workspace = %workspace.id, "cannot list pull drains");
                }
            }
        }
    }
    Ok(dependents)
}

fn replica_checkouts_of<'a>(
    registry: &'a WorkspaceRegistry,
    machine_id: &'a str,
) -> impl Iterator<Item = (&'a Workspace, &'a orbit_types::workspace::WorkspaceCheckout)> + 'a {
    registry.checkouts.iter().filter_map(move |checkout| {
        let replica_of_host = checkout.role == Some(WorkspaceCheckoutRole::Replica)
            && checkout.owner_machine_id.as_deref() == Some(machine_id);
        if !replica_of_host {
            return None;
        }
        find_workspace_by_id(registry, &checkout.workspace_id)
            .map(|workspace| (workspace, checkout))
    })
}

/// Owners of this machine's replica checkouts: the hosts it pulls from.
pub(super) fn replica_owners(global_root: &Path) -> BTreeSet<String> {
    let Ok(load) = load_registry_from_read_only(&registry_path_for(global_root)) else {
        return BTreeSet::new();
    };
    load.registry
        .checkouts
        .iter()
        .filter(|checkout| checkout.role == Some(WorkspaceCheckoutRole::Replica))
        .filter_map(|checkout| checkout.owner_machine_id.clone())
        .collect()
}
