//! Task-update requests.

use orbit_types::task::{ExternalRef, TaskComment, TaskDecision, TaskHistoryEntry, TaskStatus};

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
    /// Implementing crew recorded when automation persists a returned summary.
    pub implemented_by: Option<String>,
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    /// Typed decision recorded on the status event, such as the
    /// [`TaskDecision::RunTransition`] naming the run that shelved the task.
    /// A claimed worker's update does not carry it; the owner's entry then
    /// names the run in its note only.
    pub status_decision: Option<TaskDecision>,
    pub append_comments: Vec<TaskComment>,
    /// History entries recorded with the update, beside its status event.
    pub append_history: Vec<TaskHistoryEntry>,
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
    /// Notes for required tools a non-implementer dropped because its
    /// disallow list covers them. Empty when nothing was dropped.
    /// [ORB-15162]
    pub omitted_requirement_notes: Vec<String>,
    /// Operator-granted names whose exact disallow entries the implementing
    /// activity lifted for this run. Only `agent_implement` lifts any; the
    /// dispatcher removes them from the deny list the child enforces.
    pub granted_tools: Vec<String>,
    /// Tools a host switch removed from `effective_tools` that a deny-mode
    /// child must also deny. A deny-mode child ignores the legacy allowlist,
    /// so removing a name from `effective_tools` alone does not suppress it:
    /// the dispatcher adds these names to the deny list the child enforces,
    /// after any granted lift, so a grant cannot re-open a suppressed name.
    pub suppressed_tools: Vec<String>,
}
