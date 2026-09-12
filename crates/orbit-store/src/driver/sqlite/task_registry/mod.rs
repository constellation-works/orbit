//! SQLite task-registry storage split into focused schema, query, config, and store modules.
//!
//! `types` contains the public task-registry data structs.
//! `partition_id` derives, validates, and allocates task-store partition ids.
//! `schema` owns SQLite schema setup, migrations, and registry user-version guards.
//! `queries` contains internal SQL helpers and row-to-type mapping.
//! `util` contains shared path, time, relation, and WAL helpers used by the registry.
//! `store` contains the `TaskRegistryStore` implementation and transaction orchestration.
//! `tests` contains the registry unit tests; split it further if it grows past the file-size budget.

use std::path::{Path, PathBuf};

mod partition_id;
mod queries;
mod schema;
mod store;
mod util;

// Reader compatibility floor, not a counter for additive setup. Action keys
// preserve the v5 task/allocator format and can be ignored by older readers.
// Version 6 was shipped for that addition alone; schema.rs recovers it. Never
// reuse 6 for an incompatible format.
const REGISTRY_SCHEMA_VERSION: u32 = 5;

pub fn task_registry_path(global_root: &Path) -> PathBuf {
    global_root.join("tasks").join("index.sqlite")
}

/// Root directory of the per-workspace task bundle partitions
/// (`<global_root>/tasks/workspaces/<partition_id>/`), mirroring the
/// `workspaces_dir` the store itself derives from [`task_registry_path`]'s
/// parent in `store.rs`.
///
/// # Two id spaces, one spelling
///
/// Each subdirectory is named for a **task-store partition id**: the
/// `workspace_bindings.workspace_id` column of
/// `<global_root>/tasks/index.sqlite`, minted by
/// [`TaskRegistryStore::bind_workspace`] whenever a checkout binds without an
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

pub use crate::contracts::{
    AllocatorSeedOutcome, BindWorkspaceParams, DanglingRelationTarget, RegisterWorkspaceParams,
    TaskBundleBinding, TaskIndexFilter, WorkspaceBinding, WorkspaceCheckoutBinding,
};
pub use store::TaskRegistryStore;
pub(crate) use store::parse_orb_task_number;

#[cfg(test)]
mod tests;

mod action;
