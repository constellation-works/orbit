#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy execution-engine surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! v2 activity/job execution engine with template rendering and retry logic.
//!
//! Orchestrates the full lifecycle of running a v2 activity or job:
//! resolving input via Handlebars templates, dispatching through the internal
//! activity runtime, recording step results, and handling retries.
//!
//! # Role
//! Depends on `orbit-agent`, `orbit-exec`, `orbit-store`, `orbit-tools`, and
//! `orbit-common`. Consumed by `orbit-core`.
//!
//! # Key exports
//! - v2 dispatcher, job executor, and audit writer types re-exported at the
//!   crate root
//! - [`RuntimeHost`] — the single capability boundary used by job execution
//! - [`execute_deterministic_action`] — the built-in automation actions
//!   (git/PR/worktree/task-update) v2 job steps invoke
//!
//! # Dependency direction
//! orbit-common, orbit-agent, orbit-exec, orbit-store, orbit-tools
//! → `orbit-engine` → orbit-core

pub mod activity_job;
mod condition;
mod context;
mod executor;
mod template;

#[cfg(test)]
mod tests;

pub use activity_job::{
    ActivityAsset, AssetLoadError, CatalogDirectory, CatalogDirectoryList, CatalogError,
    DispatchError, DispatchOutcome, EnforcedAuditSink, JobAsset, JobOutcome,
    LinuxRuntimeWriteAuthority, ResolveError, ResolvedAgentSettings, ResolvedCliExecutor,
    ResolvedSandbox, ResolvedShellExecutor, V2ActivityCatalog, V2AuditWriter, V2DispatchInput,
    V2JobCatalog, V2SqliteSink, catalog_error_to_orbit, dispatch_error_to_orbit,
    dispatch_v2_activity, execute_job_with_resume, inject_system_crew_input, load_activity_asset,
    load_activity_catalog_asset, load_job_asset, resolve_crew_settings,
    resolve_job_catalog_refs_for_execution, resolve_job_target_refs,
    validate_catalog_activity_tools, validate_job, validate_job_deterministic_actions,
};
pub use context::{
    AGENT_INVOCATION_FAILED, AGENT_TIMEOUT, ActivityInvocationResult, CrewConfig, PrConfig,
    ResolvedActivityTools, ReviewLandingRequest, RuntimeHost, StepRecoveryAdmission,
    TaskActivityUpdate, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT,
    blocked_workflow_failure_update,
};
pub use executor::automation::vcs::review_gate;
pub use executor::automation::vcs::{
    WorktreeGcOptions, WorktreeGcResult, collect_worktrees, fetch_remote_base,
};
pub use executor::automation::{
    StateExecutionContext, execute_action as execute_deterministic_action,
};
