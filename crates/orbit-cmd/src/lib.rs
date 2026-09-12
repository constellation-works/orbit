// ORB-10016: command modules moved from orbit-core keep their documentation
// posture; the legacy surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Command-layer surfaces extracted from `orbit-core` [ORB-10016].
//!
//! Each module owns one CLI-facing command group whose implementation is a
//! pure consumer of the [`orbit_core::OrbitRuntime`] public API. Runtime
//! methods that used to be inherent `impl OrbitRuntime` blocks are exposed as
//! per-module extension traits (`*Commands`); import the trait (or
//! `orbit_cmd::prelude::*`) to call them.
//!
//! # Role
//! Depends on `orbit-core` (runtime/context) and composes it with
//! `orbit-registry` where an application needs both — never the other way
//! around. Consumed by `orbit-cli` and `orbit-web`.
//! Command groups that
//! orbit-core's runtime internals (tool hosts, engine hosts, bootstrap
//! seeding) invoke remain in `orbit_core::adapter::command`; see
//! `ARCHITECTURE.md` for the boundary.

pub mod activity_v2;
pub mod agent_rules;
pub mod diagnostics;
pub mod doctor;
pub mod migrate;
pub mod registry_routines;
pub mod registry_runtime;
pub mod task_owner;
pub mod task_store;
pub mod update;
pub mod workspace_catalog;

#[cfg(test)]
mod tests;

pub use activity_v2::{ActivityV2Commands, V2ActivityRunResult};
pub use diagnostics::DiagnosticsCommands;
pub use doctor::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
};
pub use migrate::{MigrateCommands, MigrateStatus, migrate_dry_run_at};
pub use task_store::{
    TaskStorePartitions, UnclaimedPartition, bound_partition_id, inspect_task_store_partitions,
    remove_checkout_task_stores, remove_unclaimed_task_stores, retain_task_store_on_catalog_remove,
    task_store_partition_path,
};

/// One-stop import for every runtime extension trait this crate defines.
pub mod prelude {
    pub use crate::activity_v2::ActivityV2Commands;
    pub use crate::diagnostics::DiagnosticsCommands;
    pub use crate::doctor::DoctorCommands;
    pub use crate::migrate::MigrateCommands;
}
