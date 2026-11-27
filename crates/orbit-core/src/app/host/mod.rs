//! The host app: the operator's registered remote hosts and the host's
//! headless browser kit [ORB-14448].
//!
//! Joins the Registry's host file to the federated MCP probe. Each entry
//! caches only what never changes on a host (`machine_id`, `task_prefix`) plus
//! the operator's name, SSH target and optional offline acknowledgement.
//! Reachability, version, protocol and
//! workspaces are read live from the host whenever a command asks, and never
//! written back. The local workspace projection reuses unchanged registry bytes
//! and machine validation context in one bounded process-local slot; every read
//! still checks path safety, readability and current contents. The CLI and the
//! dashboard both call these operations, so
//! validation and writes have one implementation.
//!
//! Host-registry commands start machine-global without selecting a workspace,
//! so they run outside the chokepoint in `app/exec.rs`: a surface calls [`run`]
//! or the named functions directly and keeps its own authorization. When show
//! or remove reports a remote host's dependents, the app reads the global
//! workspace registry and uses [`PullDrains`] to inspect matching registered
//! replica checkouts read-only for pending or running pull drains. Those
//! checkouts are inspection inputs, not the command's selected workspace.
//! The live probe and pull-drain opener sit above this crate; a surface passes
//! them in as [`Ports`].

pub mod browser_kit;

mod dependent;

mod doctor;

mod list;

mod mutate;

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_registry::hosts::HostEntry;
use orbit_types::workspace::{Workspace, WorkspaceCheckout};
use serde::Serialize;

pub use dependent::dependents_of;
pub use doctor::doctor_hosts_row;
pub use list::{
    entry_row, list_hosts, list_registered_hosts, require_local, show_host, show_registered_host,
};
pub use mutate::{
    HostChange, HostChangeEntry, add_host, remove_host, rename_host, set_host_offline,
};

use dependent::{host_dependents, replica_owners};

// Surface re-exports: what the CLI, dashboard and composition name from this subject.
pub use crate::bootstrap::browser_kit::{KitInspection, KitLayout, PLAYWRIGHT_VERSION};
#[cfg(target_os = "linux")]
pub use crate::bootstrap::linux_sandbox_host::probe_bwrap_fresh_for_user;
pub use crate::bootstrap::linux_sandbox_host::{
    BUNDLED_BWRAP_PATH, BUNDLED_BWRAP_VERSION, BwrapProbeOutcome, BwrapSource, probe_bwrap_fresh,
};
pub use crate::composition::{
    captured_env, compiled_compatibility, composition_env, host_resource_monitor,
    install_captured_env, pin_executable_generation_as,
};
pub use orbit_runtime::enable_github_quota_guard;

/// The live probe of a remote host, implemented above this crate.
pub trait Probe: Send + Sync {
    /// `target`'s row: its stored fields, joined with what a live probe from
    /// `caller_machine_id` reports when `probe` is set.
    fn remote_row(&self, target: &HostEntry, caller_machine_id: &str, probe: bool) -> HostRow;

    /// Probe a peer within Doctor's shorter reporting budget.
    fn doctor_row(&self, target: &HostEntry, caller_machine_id: &str) -> HostRow;

    /// What `ssh` reports about itself on one probe from `caller_machine_id`.
    fn probe_ssh(&self, ssh: &str, caller_machine_id: &str) -> Result<ProbedHost, OrbitError>;
}

/// Opens the local checkout whose pull drains a host command reads,
/// implemented above this crate.
pub trait PullDrains: Send + Sync {
    /// `checkout` of `workspace`, opened read-only.
    fn open_read_only(
        &self,
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
    ) -> Result<crate::Core, OrbitError>;
}

/// The reads a host command needs from above this crate.
#[derive(Clone, Copy)]
pub struct Ports<'a> {
    /// The live probe.
    pub probe: &'a dyn Probe,
    /// The pull-drain read.
    pub pull_drains: &'a dyn PullDrains,
}

/// What a remote host said about itself on one probe.
#[derive(Debug, Clone)]
pub struct ProbedHost {
    /// The host's machine identity.
    pub machine_id: String,
    /// The host's own `machine.name`.
    pub machine_name: Option<String>,
    /// The prefix the host mints task ids with.
    pub task_prefix: Option<String>,
    /// The host's Orbit version.
    pub binary_version: Option<String>,
    /// The host's distributed-drain protocol fingerprint.
    pub protocol_fingerprint: Option<String>,
    /// The host's operating system.
    pub host_os: Option<orbit_types::task::HostOs>,
    /// Which of `binary_version` and `protocol_fingerprint` differ from this
    /// machine's.
    pub skew_fields: Vec<&'static str>,
    /// The host's workspaces, each with its role there.
    pub workspaces: Vec<HostWorkspace>,
}

/// What the host app can be asked to do.
pub enum Command {
    /// `orbit host list`.
    List {
        /// The global root whose host file is read.
        global_root: PathBuf,
        /// Whether to probe each remote live.
        probe: bool,
    },
    /// `orbit host show`.
    Show {
        /// The global root whose host file is read.
        global_root: PathBuf,
        /// The host's name or `machine_id`.
        host: String,
    },
    /// `orbit host add`.
    Add {
        /// The global root whose host file is written.
        global_root: PathBuf,
        /// The SSH target that reaches the host.
        ssh: String,
        /// The name to register; the host's own name when `None`.
        name: Option<String>,
    },
    /// `orbit host rename`.
    Rename {
        /// The global root whose host file is written.
        global_root: PathBuf,
        /// The host's name or `machine_id`.
        host: String,
        /// The new name.
        new_name: String,
    },
    /// `orbit host offline`, or `orbit host online` when `offline` is `None`.
    SetOffline {
        /// The global root whose host file is written.
        global_root: PathBuf,
        /// The host's name or `machine_id`.
        host: String,
        /// The reason, and the RFC 3339 time the mark expires.
        offline: Option<(String, Option<String>)>,
    },
    /// `orbit host remove`.
    Remove {
        /// The global root whose host file is written.
        global_root: PathBuf,
        /// The host's name or `machine_id`.
        host: String,
        /// Remove the entry even while something routes to it.
        force: bool,
    },
}

/// What a host command produced.
pub enum Outcome {
    /// The host list.
    Listed(HostList),
    /// One host's detail.
    Shown(Box<HostDetail>),
    /// What a mutation wrote.
    Changed(Box<HostChange>),
}

/// Why a host command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The command refused or failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

/// One host as `orbit host list` and `show` report it.
#[derive(Debug, Clone, Serialize)]
pub struct HostRow {
    /// The host name.
    pub name: String,
    pub machine_id: String,
    /// `None` for the local host, which is never reached over SSH.
    pub ssh: Option<String>,
    /// The task prefix, retained as optional for JSON compatibility.
    pub task_prefix: Option<String>,
    /// This installation, read in-process.
    pub local: bool,
    /// Retained for JSON compatibility; always false.
    pub legacy: bool,
    /// Whether the live probe answered; `None` when no probe ran.
    pub reachable: Option<bool>,
    /// The active offline acknowledgement, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offline: Option<orbit_types::identity::HostOffline>,
    /// Why the live probe produced no facts for this host.
    pub error: Option<HostProbeError>,
    pub binary_version: Option<String>,
    pub protocol_fingerprint: Option<String>,
    /// The host's operating system: this machine's own, a registered entry's
    /// recorded OS, or what a live probe reported. Unknown for an entry
    /// registered before discovery reported it and not yet probed.
    pub host_os: Option<String>,
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
    /// Retained for JSON compatibility; always false.
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

/// Run one machine-global host command without selecting a workspace. A
/// dependents read may inspect matching replica checkouts read-only; this app
/// writes no audit row.
pub fn run(ports: Ports<'_>, command: Command) -> Result<Outcome, Error> {
    Ok(match command {
        Command::List { global_root, probe } => {
            Outcome::Listed(list_hosts(ports, &global_root, probe)?)
        }
        Command::Show { global_root, host } => {
            Outcome::Shown(Box::new(show_host(ports, &global_root, &host)?))
        }
        Command::Add {
            global_root,
            ssh,
            name,
        } => Outcome::Changed(Box::new(add_host(
            ports,
            &global_root,
            &ssh,
            name.as_deref(),
        )?)),
        Command::Rename {
            global_root,
            host,
            new_name,
        } => Outcome::Changed(Box::new(rename_host(&global_root, &host, &new_name)?)),
        Command::SetOffline {
            global_root,
            host,
            offline,
        } => {
            let offline = offline
                .as_ref()
                .map(|(reason, until)| (reason.as_str(), until.as_deref()));
            Outcome::Changed(Box::new(set_host_offline(&global_root, &host, offline)?))
        }
        Command::Remove {
            global_root,
            host,
            force,
        } => Outcome::Changed(Box::new(remove_host(ports, &global_root, &host, force)?)),
    })
}

/// This machine's Orbit version, as a probe compares it.
pub fn local_binary_version() -> &'static str {
    crate::application::distributed::owner_binary_version()
}

/// This machine's distributed-drain protocol fingerprint.
pub fn local_protocol_fingerprint() -> &'static str {
    orbit_store::contract::distributed_drain_protocol_fingerprint()
}

/// Run `probe` over every item at once, each within its own budget, so one
/// unreachable host never holds the others. Results keep the input order.
pub fn in_parallel<T, R>(items: &[T], probe: impl Fn(&T) -> R + Sync) -> Vec<R>
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
