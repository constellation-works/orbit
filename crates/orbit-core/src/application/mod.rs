//! Domain operations the runtime exposes over one workspace and its host.
//!
//! Each submodule owns one operation group. [`crate::OrbitRuntime`] opens
//! them; CLI, web, and command adapters call that surface. Process entry
//! and root bootstrap live outside this module.
//!
//! - Tasks, jobs, and delivery: `task`, `task_bundle`, `job`, `workflow`,
//!   `landing`, `review`, `operator_input`.
//! - Automation: `auto_tasks`, `automation`, `workspace_auto`,
//!   `sweep_filing`, `seeding_policy`.
//! - Agents and runs: `executor`, `experiment`, `resident`, `vision_probe`, `statistics`.
//! - Plugins and skills: `plugin`, `skill`, `tool_name_migration`.
//! - Workspace and host: `workspace`, `codebase`, `config`, `distributed`,
//!   `pull_drain`, `host_labels`, `gc`, `change_watch`,
//!   `managed_assets`.
//! - Policy and audit: `application_authority`, `audit_event`,
//!   `contracts`, `admission`, `backlog_forecast`,
//!   `tag_instructions`.
//! - `test_support` is compiled only for tests.

pub(crate) mod admission;
mod application_authority;
pub mod audit_event;
pub mod auto_tasks;
pub mod automation;
pub(crate) mod backlog_forecast;
pub mod change_watch;
pub mod codebase;
pub mod config;
pub mod contracts;
pub mod distributed;
pub(crate) mod executor;
pub mod experiment;
pub mod gc;
pub(crate) mod host_labels;
pub mod job;
pub mod landing;
pub(crate) mod managed_assets;
pub mod operator_input;
pub mod plugin;
pub(crate) mod pull_drain;
pub mod resident;
pub mod review;
pub mod seeding_policy;
pub mod skill;
pub(crate) mod statistics;
pub mod store_opener;
pub(crate) mod sweep_filing;
pub(crate) mod tag_instructions;
pub mod task;
pub mod task_bundle;

pub(crate) mod tool_name_migration;
pub mod vision_probe;
pub(crate) mod workflow;
mod workspace;
pub(crate) mod workspace_auto;
pub(crate) use workspace::product_profile as workspace_product_profile;
pub use workspace::{
    live_runs as workspace_live_runs, preset as workspace_preset, sync as workspace_sync,
};
// The env variant's only caller is the Linux-only process-cleanup test, so
// its re-export carries the same target gate.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use tests::application::run_isolated_test_with_env;

// Mutable adapter fixtures share the application fixtures' isolation launcher.
#[cfg(test)]
pub(crate) use tests::application::run_isolated_test;

/// Audit identity used for system-initiated (non-agent) mutations.
/// `pub` because the direct v2 activity runner moved to `orbit-cmd`
/// [ORB-10016] and stamps the same identity.
pub const SYSTEM_AUDIT_IDENTITY: &str = "system";

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
