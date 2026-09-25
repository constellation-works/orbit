//! Command implementations for all Orbit CLI subcommands.
//!
//! Each sub-module (task, job, activity, skill, audit, tool, init)
//! provides the data types and logic for one command group. Commands are
//! executed via the `Execute` trait, which receives an `&OrbitRuntime` and
//! produces an `OrbitError` on failure.
//!
//! The `init` module is special: it also provides `execute_without_runtime`
//! for bootstrapping a new Orbit root before a runtime can be constructed.
//! Default YAML assets (e.g., sample skills, config templates) are embedded
//! at compile time via `include_str!` and seeded to disk on first `orbit init`.

/// Audit identity used for system-initiated (non-agent) mutations.
/// `pub` because the direct v2 activity runner moved to `orbit-cmd`
/// [ORB-10016] and stamps the same identity.
pub const SYSTEM_AUDIT_IDENTITY: &str = "system";

pub mod audit_event;
pub mod auto_tasks;
pub mod config;
pub mod distributed;
pub mod epic_retirement;
pub(crate) mod executor;
pub mod gc;
pub mod health;
pub mod job;
pub mod landing;
pub(crate) mod managed_assets;
pub mod plugin;
pub mod review;
pub mod routines;
pub(crate) mod search;
pub mod skill;
pub mod task;
pub(crate) mod workflow;
pub mod workspace_sync;

#[cfg(test)]
mod tests;

pub mod automation;
