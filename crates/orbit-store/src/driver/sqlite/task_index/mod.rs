//! The task index SQLite database (`tasks/index.sqlite`): its file name, open
//! path, connection pragmas, read pool and frozen schema steps. The SQL that
//! reads and writes its rows lives in `crate::repository::task_index`.

mod connection;
pub(crate) mod schema;

pub(crate) use connection::TaskIndexDb;
pub(crate) use schema::open::HIGH_WATER_UNKNOWN_AUTHORITY;
pub(crate) use schema::registry_compatibility;
use std::path::{Path, PathBuf};

/// File name of the task index database inside `<global_root>/tasks/`.
pub const FILE_NAME: &str = "index.sqlite";

pub fn task_registry_path(global_root: &Path) -> PathBuf {
    global_root.join("tasks").join(FILE_NAME)
}

/// Root directory of the per-workspace task bundle partitions
/// (`<global_root>/tasks/workspaces/<partition_id>/`), mirroring the
/// `workspaces_dir` the store itself derives from [`task_registry_path`]'s
/// parent in `connection.rs`.
///
/// # Two id spaces, one spelling
///
/// Each subdirectory is named for a **task-store partition id**: the
/// `workspace_bindings.workspace_id` column of
/// `<global_root>/tasks/index.sqlite`, minted by
/// `TaskRegistryStore::bind_workspace` whenever a checkout binds without an
/// explicit id. The **workspace-registry id** is a different namespace with a
/// different owner: `Workspace.id` in `<global_root>/workspaces.json`, minted
/// as `ws_<slug>` by `orbit workspace init`, which then passes that `ws_*` id
/// back in as the partition id — so for a workspace created that way the two
/// ids read identically and look like one value.
///
/// They are not. These partition ids have no workspace-registry row at all:
///
/// - legacy `<slug>-<hash>` ids (for example `orbit-5c61b3`), minted here for
///   every checkout that bound before `workspace init` supplied an id;
/// - `ws_unbound-data-dir`, the synthetic partition every `--root <data-dir>`
///   write lands in.
///
/// Code that reads both registries must therefore resolve a partition's owner
/// through this registry. Comparing these directory names against the
/// workspace registry made 18 of 22 live partitions look orphaned and turned
/// the doctor repair into a command that would have deleted the host's whole
/// task store [ORB-12109 / ORB-12119].
pub fn task_workspaces_dir(global_root: &Path) -> PathBuf {
    global_root.join("tasks").join("workspaces")
}
