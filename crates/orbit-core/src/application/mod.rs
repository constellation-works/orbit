//! Domain operations the runtime exposes over one workspace and its host.
//!
//! Each submodule owns one operation group. [`crate::OrbitRuntime`] opens
//! them; CLI, web, and command adapters call that surface. Process entry
//! and root bootstrap live outside this module.
//!
//! - Tasks, jobs, and delivery: `task`, `task_bundle`, `job`, `workflow`,
//!   `landing`, `review`, `notice`, `operator_input`.
//! - Automation: `auto_tasks`, `automation`, `workspace_auto`,
//!   `sweep_filing`, `seeding_policy`.
//! - Agents and runs: `agents`, `executor`, `agent_corpus`, `crew_memory`,
//!   `evolution`, `experiment`, `resident`, `vision_probe`, `statistics`.
//! - Plugins: `plugin`, `tool_name_migration`.
//! - Workspace and host: `codebase`, `distributed`, `pull_drain`,
//!   `host_labels`, `gc`, `change_watch`, `managed_assets`.
//! - Policy and search: `application_authority`, `contracts`, `admission`,
//!   `search`, `backlog_forecast`, `tag_instructions`.
//! - `test_support` is compiled only for tests.

pub(crate) mod admission;
pub(crate) mod agent_corpus;
pub mod agents;
mod application_authority;
pub mod auto_tasks;
pub mod automation;
pub(crate) mod backlog_forecast;
pub mod change_watch;
pub mod codebase;
pub mod contracts;
pub mod crew_memory;
pub mod distributed;
pub mod evolution;
pub(crate) mod executor;
pub mod experiment;
pub mod gc;
pub(crate) mod host_labels;
pub mod job;
pub mod landing;
pub(crate) mod managed_assets;
pub mod notice;
pub mod operator_input;
pub mod plugin;
pub(crate) mod pull_drain;
pub mod resident;
pub mod review;
pub(crate) mod search;
pub mod seeding_policy;
pub(crate) mod statistics;
pub mod store_opener;
pub(crate) mod sweep_filing;
pub(crate) mod tag_instructions;
pub mod task;
pub mod task_bundle;

pub(crate) mod tool_name_migration;
pub mod vision_probe;
pub(crate) mod workflow;
pub(crate) mod workspace_auto;

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
