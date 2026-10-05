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
    DispatchError, DispatchOutcome, JobOutcome, LinuxRuntimeWriteAuthority, ResolvedCliExecutor,
    ResolvedSandbox, ResolvedShellExecutor, SandboxMask, V2AuditWriter, V2DispatchInput,
    V2SqliteSink, dispatch_error_to_orbit, dispatch_v2_activity, execute_job_with_resume,
    inject_system_crew_input, load_activity_asset, resolve_crew_settings,
    resolve_job_catalog_refs_for_execution, validate_job, validate_job_deterministic_actions,
};
pub use context::{
    ClaimExecutionContext, CrewConfig, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, HandoffLandingContext, HandoffLandingStep,
    HandoffLandingUpdate, PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun, PrConfig,
    RebaseRecoveryAttemptScope, ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest,
    ReviewerInvocationRequest, RuntimeHost, STEP_RECOVERY_DECISION_SCHEMA_VERSION,
    StepRecoveryDecisionRead, StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
    StepRecoveryVerdict, TaskActivityUpdate, TaskAutomationUpdate, WORKFLOW_RUN_FAILED_EVENT,
    WORKFLOW_RUN_INTERRUPTED_EVENT, WorktreeGcTaskLookup, blocked_workflow_failure_update,
    blocked_workflow_interruption_update,
};
pub use executor::automation::execute_action as execute_deterministic_action;
pub use executor::automation::vcs::claim::{observe_candidate, observe_published_candidate};
pub use executor::automation::vcs::review_gate;
pub use executor::automation::vcs::{
    WorktreeGcOptions, WorktreeGcResult, collect_worktrees, fetch_remote_base,
    run_worktree_has_build_output, validate_claim_new_paths,
};
