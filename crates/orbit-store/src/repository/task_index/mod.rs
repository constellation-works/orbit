//! The task registry repository: the SQL and row mapping over
//! `tasks/index.sqlite`, split into focused query, config, and store modules.
//!
//! The file itself (its name, open call, pragmas and schema steps) belongs to
//! the `crate::driver::sqlite::task_index` driver; every module here reaches
//! it through that driver's read and write path.
//!
//! `partition_id` derives, validates, and allocates task-store partition ids.
//! `queries` contains internal SQL helpers and row-to-type mapping.
//! `util` contains shared path, time and relation helpers used by the registry.
//! `store` defines `TaskRegistryStore` over the driver handle; the rest of its
//! implementation is split by concern: `workspaces` (checkout and workspace bindings), `bindings`
//! (task-bundle bindings), `index` (task index rows and projections) and `allocator`
//! (task-id allocation and prefix authority).
//! `envelope_stamps` persists the freshness scan's envelope stamps, a derived
//! cache that spares a cold listing from parsing every envelope.
//! `decision_snapshots` persists stamp-bound closed decisions and the request
//! owner index; open requests are always re-read from their bundles.
//! `relations` validates task relations: target existence, dangling targets, and cycles.
//! `listing` contains the index reads behind bounded task listing: freshness rows,
//! filtered selection, and the workspace-scoped status projection.
//! `index::snapshot` reuses freshness rows until a registry commit invalidates them.
//! `change_stamps` reuses task change stamps until a registry commit invalidates them.
//! `tests` contains the registry unit tests, split to mirror the source modules.

mod action;
mod allocator;
mod binding;
mod change_stamps;
mod decision_snapshots;
mod envelope_stamps;
mod index;
mod listing;
mod partition_id;
mod queries;
mod relations;
mod store;
mod util;
mod workspaces;

pub use crate::contract::{
    AllocatorSeedOutcome, BindWorkspaceParams, DanglingRelationTarget, IndexedTaskRow,
    RegisterWorkspaceParams, TaskBundleBinding, TaskIndexFilter, TaskIndexSelection,
    WorkspaceBinding, WorkspaceCheckoutBinding,
};
pub use crate::driver::sqlite::task_index::{task_registry_path, task_workspaces_dir};
pub(crate) use allocator::parse_orb_task_number;
pub(crate) use index::{TaskIndexProofRow, TaskIndexSnapshot};
pub use store::TaskRegistryStore;

#[cfg(test)]
mod tests;
