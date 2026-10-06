//! Task-update requests and unavailable-capability errors.

use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, TaskComment, TaskStatus};

use crate::activity_job::DispatchError;

#[derive(Debug, Clone, Default)]
pub struct TaskAutomationUpdate {
    /// Status observed by a decision that must not overwrite a later operator transition.
    pub expected_status: Option<TaskStatus>,
    pub status: Option<TaskStatus>,
    pub plan: Option<String>,
    /// Default `None` = leave the task's `context_files` untouched. `Some(v)`
    /// replaces the field wholesale (mirrors `TaskDocumentUpdateParams.context_files`
    /// semantics in `orbit-store`). Only set deliberately — most automation
    /// call sites should leave this at `None`.
    pub context_files: Option<Vec<String>>,
    pub external_refs: Vec<ExternalRef>,
    pub execution_summary: Option<String>,
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    pub append_comments: Vec<TaskComment>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub job_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskActivityUpdate {
    pub status: TaskStatus,
    /// Status observed before the activity requested this write. The runtime
    /// compares it under the task lock so a delayed worker cannot overwrite a
    /// newer operator decision.
    pub expected_status: TaskStatus,
    pub execution_summary: Option<String>,
    pub comment: Option<String>,
    pub note: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    /// Trusted completion activity's owning run, exempted while that run
    /// performs its own final transition. Its recorded children are not exempt.
    pub calling_run_id: Option<String>,
}

/// Task requirements and the resulting activity allowlist fixed at admission.
/// In deny mode `effective_tools` is the concrete callable set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedActivityTools {
    pub requested_tools: Vec<String>,
    pub effective_tools: Vec<String>,
}

pub(super) fn unsupported_runtime_capability(capability: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}

pub(super) fn unsupported_dispatch_capability(capability: &str) -> DispatchError {
    DispatchError::JobExecution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}
