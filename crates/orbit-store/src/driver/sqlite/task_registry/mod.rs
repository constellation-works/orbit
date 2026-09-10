//! SQLite task-registry storage split into focused schema, query, config, and store modules.
//!
//! `types` contains the public task-registry data structs.
//! `workspace_id` derives, validates, and allocates workspace identifiers.
//! `schema` owns SQLite schema setup, migrations, and registry user-version guards.
//! `queries` contains internal SQL helpers and row-to-type mapping.
//! `util` contains shared path, time, relation, and WAL helpers used by the registry.
//! `store` contains the `TaskRegistryStore` implementation and transaction orchestration.
//! `tests` contains the registry unit tests; split it further if it grows past the file-size budget.

use std::path::{Path, PathBuf};

mod queries;
mod schema;
mod store;
mod util;
mod workspace_id;

// Reader compatibility floor, not a counter for additive setup. Action keys
// preserve the v5 task/allocator format and can be ignored by older readers.
// Version 6 was shipped for that addition alone; schema.rs recovers it. Never
// reuse 6 for an incompatible format.
const REGISTRY_SCHEMA_VERSION: u32 = 5;

pub fn task_registry_path(global_root: &Path) -> PathBuf {
    global_root.join("tasks").join("index.sqlite")
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
