//! The task lifecycle: which status changes are legal, and what each one
//! requires before it may be written.
//!
//! [ORB-12245] `orbit.task.update` used to be a free status setter. Any agent
//! could move a `proposed` task straight to `done` — no plan, no run, no
//! review — and move it back again, while `task.start` and the delivery
//! pipeline enforced real rules on the paths Orbit itself drives. The rules
//! now live here, and every attributed surface (the CLI `task update`, the
//! dashboard, and the registered `orbit.task.update` tool) routes its status
//! changes through them.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::JobRunState;
use std::collections::HashSet;

use crate::OrbitRuntime;
use crate::application::job::running_run_has_verified_owner;

use super::params::TaskUpdateParams;

const UNAUTHORED_TASK_PLAN_PLACEHOLDER: &str = "To be authored by executing agent at start time.";

/// Status event recorded when a human overrides the lifecycle table from the
/// bare CLI or the dashboard, so an audit can tell a governed transition from
/// an override.
pub(crate) const FORCED_STATUS_EVENT: &str = "forced";

/// Whether `from -> to` is a legal lifecycle edge.
///
/// The shape mirrors the diagram agents are given in the `orbit` skill:
///
/// ```text
/// proposed → backlog → in-progress → review → done
///          ↘ rejected
/// someday  → in-progress ; blocked → backlog | in-progress
/// review   → backlog | in-progress | rejected
/// rejected → backlog | in-progress   (reconsider)
/// *        → blocked | archived      (from any open status)
/// ```
///
/// Two rules carry the governance weight. `done` is reachable only from
/// `review`, so completion always follows work that was offered for review;
/// and `done`/`archived` are terminal, so delivered or shelved work is not
/// quietly reopened — a regression gets a new task with a `regression_from`
/// relation. Reconsidering a rejection is the single exception, because a
/// rejection closes a proposal rather than recording delivery.
pub fn task_status_transition_allowed(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::{
        Archived, Backlog, Blocked, Done, InProgress, Proposed, Rejected, Review, Someday,
    };

    if from == to {
        return true;
    }

    match (from, to) {
        (Done | Archived, _) => false,
        (Rejected, target) => matches!(target, Backlog | InProgress),
        // An executor must be able to block from anywhere it can still run,
        // and any open task can be shelved.
        (_, Blocked | Archived) => true,
        (Blocked, target) => matches!(target, Backlog | InProgress),
        (Proposed, target) => matches!(target, Backlog | Someday | InProgress | Rejected),
        (Backlog, target) => matches!(target, Proposed | Someday | InProgress | Rejected),
        (Someday, target) => matches!(target, Backlog | InProgress | Rejected),
        (InProgress, target) => matches!(target, Backlog | Someday | Review | Rejected),
        (Review, target) => matches!(target, Backlog | InProgress | Done | Rejected),
    }
}

/// The companion field an otherwise-legal transition still needs from its
/// caller, if the task does not already carry equivalent evidence.
///
/// This is the read-side counterpart of [`ensure_status_change_allowed`]. UI
/// projections use it to collect evidence before submitting a mutation while
/// the guarded update path remains the authority that accepts or refuses it.
pub fn task_status_transition_required_field(
    runtime: &OrbitRuntime,
    task: &Task,
    target: TaskStatus,
) -> Result<Option<&'static str>, OrbitError> {
    if !task_status_transition_allowed(task.status, target) || task.status == target {
        return Ok(None);
    }

    match target {
        TaskStatus::InProgress if in_progress_transition_requires_plan(task.status) => {
            let plan = task.plan.trim();
            if plan.is_empty() || plan == UNAUTHORED_TASK_PLAN_PLACEHOLDER {
                Ok(Some("plan"))
            } else {
                Ok(None)
            }
        }
        TaskStatus::Done
            if !completion_evidence_present(runtime, task, &TaskUpdateParams::default())? =>
        {
            Ok(Some("execution_summary"))
        }
        _ => Ok(None),
    }
}

/// Refuse a status change the lifecycle does not allow, or one whose
/// preconditions the task does not yet meet.
///
/// `params` is the same write that carries the status, so a caller may supply
/// the missing plan or execution summary in the call that transitions — the
/// precondition is read from the task the write would produce, not from the
/// one it started with.
pub(crate) fn ensure_status_change_allowed(
    runtime: &OrbitRuntime,
    task: &Task,
    params: &TaskUpdateParams,
    target: TaskStatus,
) -> Result<(), OrbitError> {
    let from = task.status;
    if from == target {
        return Ok(());
    }
    if !task_status_transition_allowed(from, target) {
        return Err(refused(
            &task.id,
            from,
            target,
            unreachable_reason(from, target),
        ));
    }

    if target == TaskStatus::Done {
        ensure_completion_run_stopped(
            runtime,
            task,
            params.job_run_id.as_ref().and_then(|id| id.as_deref()),
            None,
        )?;
    }

    match target {
        TaskStatus::InProgress if in_progress_transition_requires_plan(from) => {
            let plan = params.plan.as_deref().unwrap_or(task.plan.as_str());
            ensure_task_has_execution_plan(&task.id, plan)
        }
        TaskStatus::Done if !completion_evidence_present(runtime, task, params)? => Err(refused(
            &task.id,
            from,
            target,
            "completion requires a non-empty execution summary, or a job run ID whose run \
             finished successfully",
        )),
        _ => Ok(()),
    }
}

/// Refuse completion while a linked implementation run (or a recorded child)
/// still has its original owner. The trusted delivery activity exempts only
/// its own run: that run necessarily remains Running until its last step exits.
/// A simultaneous replacement of `job_run_id` cannot hide the original link.
pub(crate) fn ensure_completion_run_stopped(
    runtime: &OrbitRuntime,
    task: &Task,
    replacement_run_id: Option<&str>,
    calling_run_id: Option<&str>,
) -> Result<(), OrbitError> {
    let mut pending = Vec::new();
    pending.extend(task.job_run_id.clone());
    pending.extend(replacement_run_id.map(str::to_string));
    let mut visited = HashSet::new();

    while let Some(run_id) = pending.pop() {
        let run_id = run_id.trim();
        if run_id.is_empty() || !visited.insert(run_id.to_string()) {
            continue;
        }
        let Some(run) = runtime.get_job_run_backend(run_id)? else {
            continue;
        };
        if Some(run_id) != calling_run_id && running_run_has_verified_owner(&run) {
            return Err(OrbitError::TaskCompletionLiveRun {
                task_id: task.id.clone(),
                run_id: run_id.to_string(),
            });
        }
        if let Some(state) = runtime.read_run_state(run_id)? {
            pending.extend(
                state
                    .child_dispatches
                    .into_iter()
                    .map(|child| child.child_run_id),
            );
        }
    }
    Ok(())
}

/// The precondition a refused edge is missing, in the terms of the pair that
/// was asked for rather than of the table that refused it.
fn unreachable_reason(from: TaskStatus, to: TaskStatus) -> &'static str {
    match (from, to) {
        (_, TaskStatus::Done) => "'done' is reachable only from 'review'",
        (TaskStatus::Done | TaskStatus::Archived, _) => {
            "delivered and archived work does not reopen; file a new task with a \
             'regression_from' relation instead"
        }
        (TaskStatus::Rejected, _) => {
            "a rejection is only reconsidered back to 'backlog' or 'in-progress'"
        }
        _ => "the lifecycle has no edge between these statuses",
    }
}

fn refused(id: &str, from: TaskStatus, to: TaskStatus, reason: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "task '{id}' cannot move from '{from}' to '{to}': {reason}"
    ))
}

/// Whether the task carries evidence that the work it claims to complete was
/// actually performed: an execution summary someone wrote, or a job run that
/// finished successfully.
fn completion_evidence_present(
    runtime: &OrbitRuntime,
    task: &Task,
    params: &TaskUpdateParams,
) -> Result<bool, OrbitError> {
    let summary = params
        .execution_summary
        .as_deref()
        .unwrap_or(task.execution_summary.as_str());
    if !summary.trim().is_empty() {
        return Ok(true);
    }

    let job_run_id = match &params.job_run_id {
        Some(replacement) => replacement.as_deref(),
        None => task.job_run_id.as_deref(),
    };
    let Some(job_run_id) = job_run_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(false);
    };

    Ok(runtime
        .get_job_run_backend(job_run_id)?
        .is_some_and(|run| run.state == JobRunState::Success))
}

pub(crate) fn ensure_task_has_execution_plan(id: &str, plan: &str) -> Result<(), OrbitError> {
    let normalized = plan.trim();
    if normalized.is_empty() || normalized == UNAUTHORED_TASK_PLAN_PLACEHOLDER {
        return Err(OrbitError::InvalidInput(format!(
            "task '{id}' requires a non-empty execution plan before transitioning to in-progress"
        )));
    }
    Ok(())
}

/// `backlog` and `in-progress` are the statuses a managed run picks work up
/// from, where the plan is authored by the executing agent at start time.
/// Every other source is a human or agent starting work directly, which must
/// bring a plan with it.
pub(crate) fn in_progress_transition_requires_plan(from_status: TaskStatus) -> bool {
    !matches!(from_status, TaskStatus::Backlog | TaskStatus::InProgress)
}
