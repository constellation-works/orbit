//! `orbit host`: the operator's registered remote hosts [ORB-14448].
//!
//! Joins the Registry's host file to the federated MCP probe. Each entry
//! caches only what never changes on a host (`machine_id`, `task_prefix`) plus
//! the operator's name and SSH target; reachability, version, protocol and
//! workspaces are read live from the host whenever a command asks, and never
//! written back. The CLI and the dashboard both call these operations, so
//! validation, migration and writes have one implementation.

mod dependents;
mod doctor;
mod list;
mod mutate;
mod probe;
mod remote;
mod route;

use std::path::PathBuf;

use serde::Serialize;

pub use dependents::dependents_of;
pub use doctor::doctor_hosts_row;
pub use list::{list_hosts, list_registered_hosts, show_host, show_registered_host};
pub use mutate::{HostChange, HostChangeEntry, add_host, remove_host, rename_host};
pub use probe::local_host_facts;
pub use route::{
    HostWorkspaceRoute, TaskIdRoute, host_ssh_target, local_mirror_workspace, remote_task_holder,
    resolve_host_workspace, route_task_id, routed_client, selector_remote_host, task_prefix_remote,
};

// Shared with the sibling modules through `super::`.
use dependents::{host_dependents, replica_owners};
use list::require_local;
use probe::{in_parallel, local_binary_version};
use remote::{RemoteTarget, remote_row, remote_targets, remote_workspace, skew_fields};

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
