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
//! per-module extension traits (`*Commands`); import the trait from the crate
//! root to call them.
//!
//! # Role
//! Depends on `orbit-core` (runtime/context) and composes it with
//! `orbit-registry` where an application needs both — never the other way
//! around. Consumed by `orbit-cli` and `orbit-web`.
//! Command groups that
//! orbit-core's runtime internals (tool hosts, engine hosts, bootstrap
//! seeding) invoke remain in `orbit_core::adapter::command`; see
//! `ARCHITECTURE.md` for the boundary.

pub mod agent_rules;
mod diagnostics;
mod doctor;
pub mod hosts;
pub mod mcp_clients;
mod migrate;
mod registry;
mod task;
pub use registry::{routines as registry_routines, runtime as registry_runtime};
pub use task::{owner as task_owner, store as task_store};
pub mod update;
mod workspace_catalog;

#[cfg(test)]
mod tests;

pub use diagnostics::DiagnosticsCommands;
pub use doctor::{
    DoctorCommands, DoctorProbe, OrphanTaskStoreRemoval, WorkspaceDoctorResult,
    WorkspaceDoctorStatus, doctor_report_probes, doctor_row_json, provider_limit_findings,
    run_doctor_report,
};
pub use migrate::{MigrateCommands, MigrateStatus, migrate_dry_run_at};
pub use task_store::{
    bound_partition_id, checkout_task_store_partitions, remove_checkout_task_stores,
    retain_task_store_on_catalog_remove, task_store_partition_path,
};

mod worker_coordination;

/// Materialize executor-local artifact bytes before an owner coordination call.
pub use orbit_tools::prepare_remote_task_artifact_put;
