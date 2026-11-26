//! What a drain's leaves did, for `orbit run show`.
//!
//! A drain's own state answers "did the coordinator run", not "did the work
//! ship": it dispatches its leaves detached and never observes their outcomes,
//! so a drain whose every leaf failed still finishes as `success`. This
//! summary reads each leaf's own run, plus the drain's last admission pass, so
//! the operator sees admitted / succeeded / failed / still-waiting in one
//! place. It is additive: the drain's state and every existing field are
//! unchanged.

use crate::application::job::{job_run_task_ids, run_error_step};
use crate::{JobRun, OrbitRuntime};
use orbit_types::workflow::{
    DrainAdmissionPass, DrainApprovalReport, DrainCapacity, DrainWaitingTask, JobRunState,
    PipelineState, ResourceThrottle,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The coordinator job whose leaves this summarizes.
const DRAIN_JOB: &str = "workspace_auto_pipeline";

/// The per-task job a drain dispatches, one run per admitted task.
const LEAF_JOB: &str = "task_auto_pipeline";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrainLeafSummary {
    pub admitted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    /// Leaves that have not reached a terminal state.
    pub running: usize,
    /// Leaves whose run record could not be read.
    pub unreadable: usize,
    /// Succeeded or held leaves whose task is in review under a delivery
    /// hold or a current completion hold. Not a failure.
    pub held_in_review: usize,
    /// Delivery code or completion-hold rule, to the task ids counted there.
    pub held_in_review_by_reason: BTreeMap<String, Vec<String>>,
    pub held_leaves: Vec<HeldReviewLeaf>,
    pub failed_leaves: Vec<FailedLeaf>,
    pub waiting: WaitingBacklog,
    /// What an `--approve-proposed` drain approved and held [ORB-14117].
    pub approvals: Option<DrainApprovalReport>,
    /// Shared occupancy at the last pass, separate from this drain's outcomes.
    pub capacity: Option<DrainCapacity>,
}

impl DrainLeafSummary {
    pub fn has_failed_leaves(&self) -> bool {
        !self.failed_leaves.is_empty()
    }

    /// Whether the drain left admissible or excluded work unstarted.
    pub fn has_starved_tasks(&self) -> bool {
        self.waiting.has_starved_tasks()
    }

    pub fn to_json(&self) -> Value {
        let tasks = |tasks: &[WaitingTask]| {
            tasks
                .iter()
                .map(|task| {
                    json!({
                        "task_id": task.task_id,
                        "reason": task.reason,
                        "blocked_by": task.blocked_by,
                        "detail": task.detail,
                    })
                })
                .collect::<Vec<_>>()
        };
        json!({
            "admitted": self.admitted,
            "succeeded": self.succeeded,
            "failed": self.failed,
            "cancelled": self.cancelled,
            "running": self.running,
            "unreadable": self.unreadable,
            "held_in_review": self.held_in_review,
            "held_in_review_by_reason": self.held_in_review_by_reason,
            "held_leaves": self.held_leaves.iter().map(|leaf| json!({
                "run_id": leaf.run_id,
                "tasks": leaf.tasks.iter().map(|task| json!({
                    "task_id": task.task_id,
                    "hold": task.hold,
                    "reason": task.reason,
                    "delivery_run_id": task.delivery_run_id,
                    "pr_number": task.pr_number,
                    "field": task.field,
                    "pattern": task.pattern,
                    "matched": task.matched,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "has_failed_leaves": self.has_failed_leaves(),
            "has_starved_tasks": self.has_starved_tasks(),
            "failed_leaves": self.failed_leaves.iter().map(|leaf| json!({
                "run_id": leaf.run_id,
                "state": leaf.state.to_string(),
                "task_ids": leaf.task_ids,
                "error": leaf.error,
                "resume_run_id": leaf.resume_run_id,
            })).collect::<Vec<_>>(),
            "waiting": {
                "queued": self.waiting.queued,
                "deferred": tasks(&self.waiting.deferred),
                "deferred_total": self.waiting.deferred_count(),
                "excluded": tasks(&self.waiting.excluded),
                "excluded_total": self.waiting.excluded_total,
            },
            "resource_throttle": self.waiting.resource_throttle,
            "capacity": self.capacity,
            "approvals": self.approvals,
        })
    }

    /// The tasks this local drain's last pass left waiting for a host of
    /// another OS.
    pub fn host_os_waiting_task_ids(&self) -> impl Iterator<Item = &str> {
        self.waiting
            .excluded
            .iter()
            .filter(|task| task.reason.as_deref() == Some("host_os_mismatch"))
            .map(|task| task.task_id.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedLeaf {
    pub run_id: String,
    pub state: JobRunState,
    pub task_ids: Vec<String>,
    pub error: Option<String>,
    /// The run whose `orbit job resume` retries the failed work. A leaf fails
    /// because a run beneath it did, and resuming the wrapper only re-checks
    /// that failure, so this is the deepest resumable failed run, or the leaf
    /// itself. Set by the caller, which can read the run tree.
    pub resume_run_id: Option<String>,
}

/// One drain leaf whose task (or tasks) is held in review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldReviewLeaf {
    pub run_id: String,
    pub tasks: Vec<HeldReviewTask>,
}

/// One task on a held-in-review leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldReviewTask {
    pub task_id: String,
    pub hold: String,
    /// Delivery refusal code, or the completion-hold rule label.
    pub reason: String,
    pub delivery_run_id: String,
    pub pr_number: Option<String>,
    pub field: Option<String>,
    pub pattern: Option<String>,
    pub matched: Option<String>,
}

/// Backlog tasks the drain's last admission pass left unstarted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaitingBacklog {
    /// Admissible tasks not admitted in the last pass. Local passes include
    /// deferred tasks here; pull passes keep the owner's queue depth separate
    /// from deferred tasks. `None` when the drain recorded no pass.
    pub queued: Option<u64>,
    /// Tasks deferred by a lock conflict or owner hold, with the blocking
    /// tasks. They overlap `queued` for local passes, but not pull passes.
    pub deferred: Vec<WaitingTask>,
    /// The full count behind `deferred`, which a pull drain bounds. Records
    /// that predate it read 0, so use [`Self::deferred_count`].
    pub deferred_total: u64,
    /// Backlog tasks the drain could not admit at all, with the reason.
    pub excluded: Vec<WaitingTask>,
    pub excluded_total: u64,
    /// Host resource pressure that held the last pass [ORB-13901].
    pub resource_throttle: Option<ResourceThrottle>,
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the lists above were read: the owner's answer, for a pull drain;
    /// for a local one, the last completed pass, set only while its latest
    /// pass was deferred on a busy lock.
    pub waiting_recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether deferred tasks are included in `queued` (local pass semantics).
    pub deferred_in_queued: bool,
    /// Tasks kept off this host by reason code, whole (pull drains).
    pub by_reason: BTreeMap<String, u64>,
    /// Consecutive owner answers that kept tasks off this host and claimed
    /// none (pull drains).
    pub idle_passes: u32,
}

impl WaitingBacklog {
    pub fn has_starved_tasks(&self) -> bool {
        self.queued.is_some_and(|queued| queued > 0)
            || self.deferred_count() > 0
            || self.excluded_total > 0
    }

    /// Deferred tasks in all, listed or not.
    pub fn deferred_count(&self) -> u64 {
        self.deferred_total.max(self.deferred.len() as u64)
    }

    /// The CLI lists deferred tasks separately, so subtract them from a local
    /// pass's inclusive queued count. Pull passes already keep the two counts
    /// disjoint.
    pub fn queued_for_header(&self) -> u64 {
        let queued = self.queued.unwrap_or(0);
        if self.deferred_in_queued {
            queued.saturating_sub(self.deferred_count())
        } else {
            queued
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingTask {
    pub task_id: String,
    pub reason: Option<String>,
    pub blocked_by: Vec<String>,
    pub detail: Option<String>,
}

/// Summarize a drain's leaves, or `None` for any other run.
///
/// `read_child` resolves a dispatched run id to its record; an unreadable one
/// is counted rather than failing the whole view.
pub(super) fn summarize_drain_leaves(
    run: &JobRun,
    state: Option<&PipelineState>,
    mut read_child: impl FnMut(&str) -> Option<JobRun>,
    mut held_in_review: impl FnMut(&JobRun) -> Option<HeldReviewLeaf>,
) -> Option<DrainLeafSummary> {
    if run.job_id != DRAIN_JOB {
        return None;
    }
    let state = state?;
    let mut summary = DrainLeafSummary {
        waiting: last_pass_waiting(state),
        approvals: state.pull_drain.drain_approvals.clone(),
        capacity: state
            .pull_drain
            .drain_last_pass
            .as_ref()
            .and_then(|pass| pass.capacity.clone()),
        ..DrainLeafSummary::default()
    };
    for dispatch in state
        .child_dispatches
        .iter()
        .filter(|dispatch| dispatch.job_name == LEAF_JOB)
    {
        summary.admitted += 1;
        let Some(leaf) = read_child(&dispatch.child_run_id) else {
            summary.unreadable += 1;
            continue;
        };
        match leaf.state {
            JobRunState::Success => {
                if let Some(held) = held_in_review(&leaf) {
                    record_held_review(&mut summary, held);
                } else {
                    summary.succeeded += 1;
                }
            }
            JobRunState::Cancelled => summary.cancelled += 1,
            JobRunState::Failed | JobRunState::Timeout | JobRunState::Interrupted => {
                summary.failed += 1;
                let task_ids = leaf
                    .input
                    .as_ref()
                    .and_then(|input| input.get("task_ids"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                summary.failed_leaves.push(FailedLeaf {
                    run_id: leaf.run_id.clone(),
                    state: leaf.state,
                    task_ids,
                    error: run_error_step(&leaf).and_then(|step| step.error_message.clone()),
                    resume_run_id: None,
                });
            }
            JobRunState::Held => {
                if let Some(held) = held_in_review(&leaf) {
                    record_held_review(&mut summary, held);
                } else {
                    summary.running += 1;
                }
            }
            _ => summary.running += 1,
        }
    }
    for ids in summary.held_in_review_by_reason.values_mut() {
        ids.sort();
        ids.dedup();
    }
    if summary.admitted == 0
        && summary.waiting == WaitingBacklog::default()
        && summary.approvals.is_none()
    {
        return None;
    }
    Some(summary)
}

fn record_held_review(summary: &mut DrainLeafSummary, held: HeldReviewLeaf) {
    summary.held_in_review += 1;
    for task in &held.tasks {
        summary
            .held_in_review_by_reason
            .entry(task.reason.clone())
            .or_default()
            .push(task.task_id.clone());
    }
    summary.held_leaves.push(held);
}

/// Tasks on this leaf that are in review under a delivery hold or a current
/// completion hold. A task or run that cannot be read is left out and warned;
/// the leaf then stays in its old bucket instead of failing the drain view.
pub(super) fn held_review_leaf(runtime: &OrbitRuntime, leaf: &JobRun) -> Option<HeldReviewLeaf> {
    let mut tasks = Vec::new();
    for task_id in job_run_task_ids(leaf) {
        let task = match runtime.get_task(&task_id) {
            Ok(task) => task,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.cli.run",
                    run_id = %leaf.run_id,
                    %task_id,
                    %error,
                    "held-in-review task unreadable; leaf stays in its outcome bucket"
                );
                continue;
            }
        };
        let history = match runtime.get_task_history(&task_id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.cli.run",
                    run_id = %leaf.run_id,
                    %task_id,
                    %error,
                    "held-in-review history unreadable; leaf stays in its outcome bucket"
                );
                continue;
            }
        };
        match runtime.current_review_hold(&task, &history) {
            Ok(Some(hold)) => tasks.push(HeldReviewTask {
                task_id,
                hold: hold.kind.as_str().to_string(),
                reason: hold.code.or(hold.rule).unwrap_or_default(),
                delivery_run_id: hold.run_id,
                pr_number: hold.pr_number,
                field: hold.field,
                pattern: hold.pattern,
                matched: hold.matched,
            }),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    target: "orbit.cli.run",
                    run_id = %leaf.run_id,
                    %task_id,
                    %error,
                    "held-in-review delivery run unreadable; leaf stays in its outcome bucket"
                );
            }
        }
    }
    if tasks.is_empty() {
        None
    } else {
        Some(HeldReviewLeaf {
            run_id: leaf.run_id.clone(),
            tasks,
        })
    }
}

/// What the drain's last admission pass left waiting, from the record it keeps
/// on its own run state.
fn last_pass_waiting(state: &PipelineState) -> WaitingBacklog {
    state
        .pull_drain
        .drain_last_pass
        .as_ref()
        .map(|pass| waiting_backlog(pass, true))
        .unwrap_or_default()
}

pub fn waiting_backlog(pass: &DrainAdmissionPass, deferred_in_queued: bool) -> WaitingBacklog {
    let tasks = |tasks: &[DrainWaitingTask]| -> Vec<WaitingTask> {
        tasks
            .iter()
            .map(|task| WaitingTask {
                task_id: task.task_id.clone(),
                reason: task.reason.clone(),
                blocked_by: task.blocked_by.clone(),
                detail: task.detail.clone(),
            })
            .collect()
    };
    WaitingBacklog {
        queued: Some(pass.queued),
        deferred: tasks(&pass.deferred),
        deferred_total: pass.deferred_total,
        excluded: tasks(&pass.excluded),
        excluded_total: pass.excluded_total,
        resource_throttle: pass.resource_throttle.clone(),
        recorded_at: Some(pass.recorded_at),
        waiting_recorded_at: pass.waiting_recorded_at,
        deferred_in_queued,
        by_reason: pass.waiting_by_reason.clone(),
        idle_passes: pass.consecutive_idle_passes,
    }
}
