//! The doctor's port to the reads above this crate: the host registry, MCP
//! client files, the registered workspaces' routines, the browser kit, the
//! task-store partitions and the managed services' environments.
//!
//! The composition tier implements [`Host`] and a surface passes it in its
//! [`Command`](super::Command); the checks that read it stay here.

use std::path::{Path, PathBuf};

use orbit_common::{Env, OrbitError};
use orbit_types::identity::CrewEnvVar;

use super::{OrphanTaskStoreRemoval, WorkspaceDoctorResult};
use crate::application::routines::RoutineStatusReport;
use crate::bootstrap::browser_kit::{KitInspection, KitLayout};

/// What the doctor reads from the host through the composition tier.
pub trait Host: Send + Sync {
    /// The `hosts` row: every registered remote host and its reachability.
    fn hosts_row(&self, global_root: &Path) -> WorkspaceDoctorResult;

    /// The MCP clients that register Orbit for this workspace, as labels.
    fn registered_mcp_clients(
        &self,
        repo_root: &Path,
        workspace_id: Option<&str>,
        home_dir: Option<&Path>,
        env: &Env,
    ) -> Vec<String>;

    /// The routine status report across every registered workspace.
    fn routine_statuses(&self, global_root: &Path) -> Result<RoutineStatusReport, OrbitError>;

    /// Warnings for declared crew sources missing from the current shell or a
    /// managed service.
    fn crew_source_environment_warnings(
        &self,
        global_root: &Path,
        crews: &[(String, Vec<CrewEnvVar>)],
        env: &Env,
    ) -> Vec<String>;

    /// The browser kit under `layout` and what is wrong with it.
    fn browser_kit(&self, layout: &KitLayout, env: &Env) -> BrowserKit;

    /// `program` on the colon-separated `path`.
    fn find_in_path(&self, program: &str, path: &str) -> Option<PathBuf>;

    /// Every task-store partition on this host, classified by its claim.
    /// `None` when no partition directory exists yet.
    fn task_store_partitions(
        &self,
        global_root: &Path,
    ) -> Result<Option<TaskStorePartitions>, OrbitError>;

    /// Delete the unclaimed empty partitions and those whose checkout is
    /// confirmed gone.
    fn remove_unclaimed_task_stores(
        &self,
        global_root: &Path,
    ) -> Result<OrphanTaskStoreRemoval, OrbitError>;
}

/// The browser kit as the doctor reports it.
pub struct BrowserKit {
    /// What the kit's directory holds.
    pub inspection: KitInspection,
    /// What is wrong with an installed kit, each with its fix.
    pub problems: Vec<KitProblem>,
    /// The system libraries Chromium cannot resolve, as one problem.
    pub library_problem: Option<KitProblem>,
    /// Whether the libraries were checked at all.
    pub libraries_checked: bool,
    /// How to install Node.js when no `node` is found.
    pub node_fix: &'static str,
}

/// One thing wrong with the kit, and the command that fixes it.
pub struct KitProblem {
    /// What is wrong.
    pub problem: String,
    /// The command that fixes it.
    pub fix: String,
}

/// One task-store partition no registry claims.
pub struct TaskStorePartition {
    /// The partition directory.
    pub path: PathBuf,
    /// Task bundles directly inside it.
    pub task_bundles: usize,
}

/// A populated partition whose checkout could not be confirmed present or
/// absent.
pub struct UnreachableTaskStorePartition {
    /// The partition.
    pub partition: TaskStorePartition,
    /// The path that could not be resolved and what the filesystem said.
    pub reason: String,
}

/// What a scan of the host's task-store partitions found.
pub struct TaskStorePartitions {
    /// Partition directories present on this host.
    pub scanned: usize,
    /// Unclaimed and empty.
    pub removable: Vec<TaskStorePartition>,
    /// Bound to a checkout confirmed absent.
    pub stale: Vec<TaskStorePartition>,
    /// Unclaimed but holding task bundles.
    pub unowned: Vec<TaskStorePartition>,
    /// Bound to a checkout that could not be reached.
    pub unreachable: Vec<UnreachableTaskStorePartition>,
}
