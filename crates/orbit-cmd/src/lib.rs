//! Composition: open a [`orbit_core::Core`] for a selected workspace
//! [ORB-10016].
//!
//! Given the flags and an optional workspace selection, the registry runtime
//! factory resolves which registered checkout to open, attaches what the
//! registry knows about it to an [`orbit_core::composition::Opening`], and
//! returns the `Core` a surface passes to an app's `exec`. The remaining
//! modules are command groups and the ports the apps call back through.
//!
//! # Role
//! Depends on `orbit-core` (runtime/context) and `orbit-registry`. Core reads
//! the registry read-only for store retention; this crate still assembles
//! commands. Neither registry nor core depends on `orbit-cmd`. Consumed by
//! `orbit-cli` and `orbit-web`. Command groups that orbit-core's runtime
//! internals (tool hosts, engine hosts, bootstrap seeding) invoke remain in
//! `orbit_core::adapter::command`; see `ARCHITECTURE.md` for the boundary.

// ORB-10016: command modules moved from orbit-core keep their documentation
// posture; the legacy surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![cfg_attr(
    not(any(test, feature = "test-release-api-base")),
    deny(clippy::disallowed_methods)
)]

// A bare `cargo test` from a managed-run shell must match one from a clean shell.
#[cfg(test)]
orbit_common::isolate_test_process!();

pub mod agent_rules;

pub mod doctor_host;

pub mod hosts;

pub mod log_format;

pub mod mcp_clients;

mod migrate;

mod registry;

pub mod service_environment;

mod task;

#[cfg(feature = "test-support")]
pub mod test_support;

pub mod update;

mod worker_coordination;

mod workspace_catalog;

pub mod workspace_rebind;

pub mod workspace_removal;

pub use doctor_host::doctor_host;
pub use migrate::{MigrateStatus, migrate_dry_run_at};
/// Materialize executor-local artifact bytes before an owner coordination call.
pub use orbit_tools::prepare_remote_task_artifact_put;
pub use registry::{routines as registry_routines, runtime as registry_runtime};
pub use task::{owner as task_owner, store as task_store};
pub use task_store::{bound_partition_id, remove_checkout_task_stores, task_store_partition_path};

#[cfg(test)]
mod tests;
