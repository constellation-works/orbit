//! SQLite task-registry storage split into focused schema, query, config, and store modules.
//!
//! `types` contains the public task-registry data structs.
//! `partition_id` derives, validates, and allocates task-store partition ids.
//! `ledger` is the registry's own migration ledger, run by the store ledger engine,
//! and documents the `user_version` fence kept for pre-ledger binaries.
//! `baseline` is the frozen ledger v1 body that pre-ledger registries adopt through.
//! `index_task_type` is the frozen ledger v2 body that indexes each task's `type`.
//! `schema` adopts, migrates and admits a registry when it opens.
//! `queries` contains internal SQL helpers and row-to-type mapping.
//! `util` contains shared path, time, relation, and WAL helpers used by the registry.
//! `store` defines `TaskRegistryStore` and opens it; the rest of its implementation
//! is split by concern: `workspaces` (checkout and workspace bindings), `bindings`
//! (task-bundle bindings), `index` (task index rows and projections) and `allocator`
//! (task-id allocation and prefix authority).
//! `envelope_stamps` persists the freshness scan's envelope stamps, a derived
//! cache that spares a cold listing from parsing every envelope.
//! `relations` validates task relations: target existence, dangling targets, and cycles.
//! `listing` contains the index reads behind bounded task listing: freshness rows,
//! filtered selection, and the workspace-scoped status projection.
//! `tests` contains the registry unit tests, split to mirror the source modules.

use std::path::{Path, PathBuf};

mod allocator;
mod baseline;
mod bindings;
mod envelope_stamps;
mod index;
mod index_task_type;
pub(crate) mod ledger;
mod listing;
mod partition_id;
mod queries;
mod relations;
mod schema;
mod store;
mod util;
mod workspaces;

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
    AllocatorSeedOutcome, BindWorkspaceParams, DanglingRelationTarget, IndexedTaskRow,
    RegisterWorkspaceParams, TaskBundleBinding, TaskIndexFilter, TaskIndexSelection,
    WorkspaceBinding, WorkspaceCheckoutBinding,
};
pub(crate) use allocator::parse_orb_task_number;
pub(crate) use ledger::registry_compatibility;
pub use store::TaskRegistryStore;

#[cfg(test)]
mod tests;

mod action;
