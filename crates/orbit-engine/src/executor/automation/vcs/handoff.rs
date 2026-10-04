use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComment, TaskStatus};
use serde_json::Value;

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::input::{
    canonicalize_existing_dir, input_string_field, required_input_string, required_job_run_id,
};
use super::claim::{carries_implementer_output, implementer_summary};
use super::pr::meaningful_execution_summary;

const FAILED_HANDOFF_ACTOR: &str = "system";

/// The durable execution-summary first line that blocks delivery.
const DELIVERY_FAILED_LINE: &str = "Outcome: failed";

/// Durable delivery gate (ORB-10313 / friction F2026-07-091).
///
/// A batch commit or any PR handoff phase must stop when the task's persisted
/// execution summary reports failure on its first nonblank line — exactly
/// `Outcome: failed`. This reads the durable task record, never the advisory
/// agent response envelope, so it stays the delivery source of truth.
///
/// Empty and placeholder summaries keep their existing rejection. Other
/// meaningful summary shapes remain deliverable; only an explicit failed line
/// is authoritative enough to block Git mutation and task promotion.
pub(super) fn reject_failed_delivery(task: &Task) -> Result<(), OrbitError> {
    let Some(summary) = meaningful_execution_summary(&task.execution_summary) else {
        return Err(OrbitError::Execution(format!(
            "task '{}' requires a meaningful persisted execution_summary before delivery",
            task.id
        )));
    };

    if !reports_failure(summary) {
        return Ok(());
    }

    Err(OrbitError::Execution(format!(
        "task '{}' cannot be delivered: its persisted execution_summary begins with \
         '{DELIVERY_FAILED_LINE}'; durable delivery fails closed when the task record reports failure",
        task.id
    )))
}

/// Whether `summary`'s first nonblank line is exactly `Outcome: failed`.
pub(super) fn reports_failure(summary: &str) -> bool {
    summary.lines().map(str::trim).find(|line| !line.is_empty()) == Some(DELIVERY_FAILED_LINE)
}

/// The delivery gate for the attempt this run is delivering [ORB-13755].
///
/// A local run judges the durable task record ([`reject_failed_delivery`]).
/// A claimed leaf cannot: its implementer writes no owner task state
/// (distributed-drain design §3, "Claimed-mode implementation"), so until
/// this attempt's handoff is accepted the owner's stored summary is whatever
/// a *previous* attempt left — after a failed attempt, its `Outcome: failed`
/// settlement. Gating on that refused every claimed retry of a once-failed
/// task, however its new implementation went.
///
/// So under the trusted worker binding, for the claim's own task, a step the
/// pipeline handed this run's implementer output judges that output instead:
/// the source [`claim_handoff`](super::claim::claim_handoff) composes the
/// owner's summary from and refuses on. A current summary reporting
/// `Outcome: failed` is still refused here, before the caller touches Git.
/// A claimed implementer that reported no summary is not refused; its handoff
/// carries a generic delivery statement. Without the binding, or without
/// implementer output in the input, the durable gate applies unchanged.
pub(super) fn reject_failed_attempt<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &Task,
) -> Result<(), OrbitError> {
    let Some(current) = claimed_attempt_summary(host, input, task) else {
        return reject_failed_delivery(task);
    };
    match current {
        Some(summary) if reports_failure(&summary) => Err(OrbitError::Execution(format!(
            "task '{}' cannot be delivered: this claimed attempt's implementer summary begins \
             with '{DELIVERY_FAILED_LINE}'; a claimed leaf delivers only work its own attempt \
             reports as done",
            task.id
        ))),
        _ => Ok(()),
    }
}

/// This claimed attempt's implementer summary for `task`, when this step may
/// judge it: `None` outside a claimed leaf, for a task other than the claim's,
/// or when the step was handed no implementer output; `Some(None)` when the
/// implementer output carries no summary text.
pub(super) fn claimed_attempt_summary<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &Task,
) -> Option<Option<String>> {
    let binding = host.worker_invocation()?;
    if binding.task_id != task.id || !carries_implementer_output(input) {
        return None;
    }
    Some(implementer_summary(input))
}

pub(super) struct HandoffContext {
    pub(super) batch_id: String,
    pub(super) workspace_path: PathBuf,
    pub(super) tasks: Vec<Task>,
}

pub(super) fn load_handoff_context<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    action: &str,
) -> Result<HandoffContext, OrbitError> {
    let workspace_path = canonicalize_existing_dir(
        required_input_string(input, "workspace_path")?,
        "workspace_path",
    )?;
    let batch_id = required_job_run_id(input, action)?.to_string();
    let task_ids = completed_task_ids_from_input(input).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{action}: input.completed_task_ids must contain at least one task id"
        ))
    })?;

    let mut tasks = Vec::with_capacity(task_ids.len());
    for task_id in task_ids {
        let task = host.get_task(&task_id)?;
        if task.job_run_id.as_deref() != Some(batch_id.as_str()) {
            return Err(OrbitError::Execution(format!(
                "{action}: task '{}' no longer belongs to job run '{}'",
                task.id, batch_id
            )));
        }
        if !matches!(
            task.status,
            TaskStatus::InProgress | TaskStatus::Review | TaskStatus::Done
        ) {
            return Err(OrbitError::Execution(format!(
                "{action}: task '{}' is not promotable from status '{}'",
                task.id, task.status
            )));
        }
        reject_failed_attempt(host, input, &task)?;
        tasks.push(task);
    }

    Ok(HandoffContext {
        batch_id,
        workspace_path,
        tasks,
    })
}

pub(super) fn completed_task_ids_from_input(input: &Value) -> Option<Vec<String>> {
    let ids = input
        .get("completed_task_ids")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!ids.is_empty()).then_some(ids)
}

#[derive(Debug, Clone, Copy)]
pub(super) enum FailedHandoffPhase {
    Prepare,
    Rebase,
    Push,
    PrLookup,
    PrCreate,
    PrView,
    Promote,
    EmptyBranch,
    ObsoleteBase,
    /// The head or base no longer matches the reviewed candidate [ORB-11333].
    StaleReviewGate,
}

impl FailedHandoffPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Rebase => "rebase",
            Self::Push => "push",
            Self::PrLookup => "automation.vcs.pr.lookup",
            Self::PrCreate => "automation.vcs.pr.create",
            Self::PrView => "automation.vcs.pr.view",
            Self::Promote => "promote",
            Self::EmptyBranch => "empty-branch",
            Self::ObsoleteBase => "obsolete-base",
            Self::StaleReviewGate => "stale-review-gate",
        }
    }
}

pub(super) fn record_failed_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    input: &Value,
    phase: FailedHandoffPhase,
    error: &OrbitError,
) -> Result<(), OrbitError> {
    let header = format!(
        "pr handoff failed [run={}] [phase={}]",
        context.batch_id,
        phase.label()
    );
    let head = input_string_field(input, "head")
        .or_else(|| current_branch(&context.workspace_path))
        .unwrap_or_else(|| "unknown".to_string());
    let base = input_string_field(input, "base").unwrap_or_else(|| "main".to_string());
    let base_ref = input_string_field(input, "base_ref")
        .map(|value| format!("Base checkpoint: {value}\n"))
        .unwrap_or_default();
    let timed_out = error.to_string().contains("timed out");
    let recovery = if matches!(phase, FailedHandoffPhase::Rebase)
        && rebase_in_progress(&context.workspace_path).unwrap_or(false)
    {
        if timed_out {
            "Worktree state: rebase interrupted by a Git timeout.\n\nRecovery:\n  Timeout recovery is not conflict repair. If this attempt started the rebase, Orbit aborts it and a retry can start clean. A pre-existing or foreign rebase is left intact — inspect rebase-merge/rebase-apply before retrying. Ordinary conflict recovery remains pr_conflict_recovery; failure-handoff remains pr_failure_handoff."
        } else {
            "Worktree state: rebase stopped with unresolved conflicts.\n\nRecovery:\n  Resolve the conflicting paths in this worktree, stage them, run `git rebase --continue`, then resume the same job step. Later handoff phases have not been replayed. This is conflict recovery, not timeout recovery."
        }
    } else if timed_out {
        "Recovery:\n  This failure is a Git operation timeout, not a merge conflict and not a failure-handoff. Inspect the named checkout, then retry the same job step after timeout recovery has cleaned only owned incomplete state."
    } else {
        "Recovery:\n  Reconcile the recorded phase in this worktree, then resume the same job step. Later handoff phases have not been replayed."
    };
    let message = format!(
        "{header}\n\nHead branch: {head}\nWorktree: {worktree}\nBase branch: {base}\n{base_ref}Failing phase: {phase}\nError: {error}\n\n{recovery}",
        worktree = context.workspace_path.display(),
        phase = phase.label(),
    );

    for task in &context.tasks {
        if host
            .get_task_comments(&task.id)?
            .iter()
            .any(|comment| comment.message.starts_with(&header))
        {
            continue;
        }
        host.apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                append_comments: vec![TaskComment {
                    at: Utc::now(),
                    by: FAILED_HANDOFF_ACTOR.to_string(),
                    message: message.clone(),
                }],
                ..TaskAutomationUpdate::default()
            },
        )?;
    }
    Ok(())
}

pub(super) fn rebase_in_progress(workspace_path: &Path) -> Result<bool, OrbitError> {
    for state_dir in ["rebase-merge", "rebase-apply"] {
        let path = PathBuf::from(super::git::git_output(
            workspace_path,
            &["rev-parse", "--git-path", state_dir],
        )?);
        let path = if path.is_absolute() {
            path
        } else {
            workspace_path.join(path)
        };
        if path.is_dir() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn current_branch(workspace_path: &Path) -> Option<String> {
    super::git::git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
