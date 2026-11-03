//! Projection and diagnostics for a finished provider subprocess.

mod failure;
mod frames;
mod projection;

use std::path::Path;
use std::time::Duration;

use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;

use super::super::supervisor::CapturedOutput;
use super::limit::UsageWindowContext;
use crate::activity_job::dispatcher::ResolvedSandbox;
use crate::activity_job::workspace::WorktreeBoundaryGuard;
use crate::context::RuntimeHost;

pub(super) use frames::{provider_failure, stdout_frames};
#[cfg(test)]
pub(super) use projection::print_budget_exhausted;
pub(super) use projection::project_completion;

/// Everything a provider subprocess left behind once it exited, plus the
/// run state its completion projection reads.
pub(super) struct ProviderExit<'a> {
    pub(super) audit: &'a crate::activity_job::audit_writer::V2AuditWriter,
    pub(super) started_at: chrono::DateTime<chrono::Utc>,
    pub(super) finished_at: chrono::DateTime<chrono::Utc>,
    pub(super) host: &'a dyn RuntimeHost,
    pub(super) spec: &'a AgentLoopSpec,
    pub(super) input: &'a Value,
    pub(super) run_id: &'a str,
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) agent_execution_id: Option<i64>,
    pub(super) task_ids: &'a [String],
    pub(super) worktree_boundary: Option<WorktreeBoundaryGuard>,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) subprocess_cwd: Option<&'a Path>,
    pub(super) redaction: &'a PatternRedactor,
    pub(super) timeout_seconds: u64,
    pub(super) argv_redacted: Vec<String>,
    pub(super) stdin_blob_ref: String,
    pub(super) stdout_blob_ref: String,
    pub(super) stderr_blob_ref: String,
    pub(super) stdout: CapturedOutput,
    pub(super) stderr: CapturedOutput,
    pub(super) exit_code: Option<i32>,
    pub(super) duration: Duration,
    pub(super) timed_out: bool,
    /// The stall watch ended the process. Distinct from [`Self::timed_out`].
    pub(super) stalled: bool,
    /// The window the stall watch used. Zero when the watch was disabled.
    pub(super) stall_window: Duration,
    /// The provider-side time budget Orbit injected into argv, if the
    /// provider has one. [ORB-14683]
    pub(super) print_timeout: Option<Duration>,
    /// The account state resolved from the actual provider child environment.
    pub(super) usage_context: UsageWindowContext,
    pub(super) build_budget_waits: BuildBudgetWaits,
}
