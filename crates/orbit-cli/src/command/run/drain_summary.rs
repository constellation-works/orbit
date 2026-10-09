//! What a drain's leaves did, for `orbit run show`.
//!
//! A drain's own state answers "did the coordinator run", not "did the work
//! ship": it dispatches its leaves detached and never observes their outcomes,
//! so a drain whose every leaf failed still finishes as `success`. This
//! summary reads each leaf's own run, plus the drain's last admission pass, so
//! the operator sees admitted / succeeded / failed / still-waiting in one
//! place. It is additive: the drain's state and every existing field are
//! unchanged.

use std::collections::BTreeMap;

use orbit_core::JobRun;
use orbit_core::application::job::run_error_step;
use orbit_types::workflow::{
    DrainAdmissionPass, DrainApprovalReport, DrainCapacity, DrainWaitingTask, JobRunState,
    PipelineState, ResourceThrottle,
};
use serde_json::{Value, json};

use super::format::summarize_error_message;

/// The coordinator job whose leaves this summarizes.
const DRAIN_JOB: &str = "workspace_auto_pipeline";
/// The per-task job a drain dispatches, one run per admitted task.
const LEAF_JOB: &str = "task_auto_pipeline";

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DrainLeafSummary {
    pub(super) admitted: usize,
    pub(super) succeeded: usize,
    pub(super) failed: usize,
    pub(super) cancelled: usize,
    /// Leaves that have not reached a terminal state.
    pub(super) running: usize,
    /// Leaves whose run record could not be read.
    pub(super) unreadable: usize,
    pub(super) failed_leaves: Vec<FailedLeaf>,
    pub(super) waiting: WaitingBacklog,
    /// What an `--approve-proposed` drain approved and held [ORB-14117].
    pub(super) approvals: Option<DrainApprovalReport>,
    /// Shared occupancy at the last pass, separate from this drain's outcomes.
    pub(super) capacity: Option<DrainCapacity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FailedLeaf {
    pub(super) run_id: String,
    pub(super) state: JobRunState,
    pub(super) task_ids: Vec<String>,
    pub(super) error: Option<String>,
    /// The run whose `orbit job resume` retries the failed work. A leaf fails
    /// because a run beneath it did, and resuming the wrapper only re-checks
    /// that failure, so this is the deepest resumable failed run, or the leaf
    /// itself. Set by the caller, which can read the run tree.
    pub(super) resume_run_id: Option<String>,
}

/// Backlog tasks the drain's last admission pass left unstarted.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct WaitingBacklog {
    /// Admissible tasks not admitted in the last pass: no slot, or a lock
    /// conflict. `None` when the drain recorded no pass.
    pub(super) queued: Option<u64>,
    /// The subset of `queued` a lock conflict kept out, with the blocking tasks.
    pub(super) deferred: Vec<WaitingTask>,
    /// Backlog tasks the drain could not admit at all, with the reason.
    pub(super) excluded: Vec<WaitingTask>,
    pub(super) excluded_total: u64,
    /// Host resource pressure that held the last pass [ORB-13901].
    pub(super) resource_throttle: Option<ResourceThrottle>,
    pub(super) recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the owner gave the answer the lists above report, for a pull
    /// drain; unset for a local one, which classifies afresh each pass.
    pub(super) waiting_recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Tasks kept off this host by reason code, whole (pull drains).
    pub(super) by_reason: BTreeMap<String, u64>,
    /// Consecutive owner answers that kept tasks off this host and claimed
    /// none (pull drains).
    pub(super) idle_passes: u32,
}

/// Consecutive idle owner answers after which a pull drain says why it
/// claims nothing, rather than leaving the per-task lines to be added up.
const IDLE_SUMMARY_PASSES: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WaitingTask {
    pub(super) task_id: String,
    pub(super) reason: Option<String>,
    pub(super) blocked_by: Vec<String>,
    pub(super) detail: Option<String>,
}

impl DrainLeafSummary {
    fn has_failed_leaves(&self) -> bool {
        !self.failed_leaves.is_empty()
    }

    /// Whether the drain left admissible or excluded work unstarted.
    fn has_starved_tasks(&self) -> bool {
        self.waiting.has_starved_tasks()
    }

    pub(super) fn to_json(&self) -> Value {
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
                "excluded": tasks(&self.waiting.excluded),
                "excluded_total": self.waiting.excluded_total,
            },
            "resource_throttle": self.waiting.resource_throttle,
            "capacity": self.capacity,
            "approvals": self.approvals,
        })
    }

    /// The `Leaves:` line, then a warning per failed or starved outcome.
    pub(super) fn lines(&self, drain_state: JobRunState) -> Vec<String> {
        use crate::output::color::bold;
        let mut lines = vec![format!(
            "{} admitted={} succeeded={} failed={} running={} cancelled={}{}",
            bold("Leaves:"),
            self.admitted,
            self.succeeded,
            self.failed,
            self.running,
            self.cancelled,
            if self.unreadable > 0 {
                format!(" unreadable={}", self.unreadable)
            } else {
                String::new()
            },
        )];
        if let Some(capacity) = &self.capacity {
            lines.push(format!(
                "{} occupied={} inherited={} limit={} (last admission pass {})",
                bold("Capacity:"),
                capacity.active_leaf_runs,
                capacity.inherited_leaf_runs,
                capacity.max_active_leaf_runs,
                self.waiting
                    .recorded_at
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_default(),
            ));
        }
        if self.has_failed_leaves() {
            let note = if drain_state == JobRunState::Success {
                " (the drain's own `success` only means it ran; it does not observe its leaves)"
            } else {
                ""
            };
            lines.push(format!(
                "{} {} of {} leaves failed{note}",
                bold("WARNING:"),
                self.failed_leaves.len(),
                self.admitted,
            ));
            for leaf in &self.failed_leaves {
                let tasks = if leaf.task_ids.is_empty() {
                    "-".to_string()
                } else {
                    leaf.task_ids.join(",")
                };
                lines.push(format!(
                    "  Failed leaf {} task={tasks} state={} error={}",
                    leaf.run_id,
                    leaf.state,
                    summarize_error_message(leaf.error.as_deref()),
                ));
                lines.push(match &leaf.resume_run_id {
                    Some(resume) => format!(
                        "    retry: `orbit job resume {resume}` (or move the task back to backlog)"
                    ),
                    None => "    retry: move the task back to backlog".to_string(),
                });
            }
        }
        if let Some(throttle) = self.waiting.resource_throttle.as_ref() {
            lines.push(throttle_line(throttle, self.waiting.recorded_at));
        }
        if let Some(approvals) = &self.approvals {
            lines.extend(approval_lines(approvals));
        }
        lines.extend(self.waiting.lines());
        lines
    }
}

impl WaitingBacklog {
    fn has_starved_tasks(&self) -> bool {
        self.queued.is_some_and(|queued| queued > 0)
            || !self.deferred.is_empty()
            || self.excluded_total > 0
    }

    /// The `Still waiting:` block, and for a pull drain that has found nothing
    /// claimable here several passes running, the `idle:` line that says why.
    fn lines(&self) -> Vec<String> {
        use crate::output::color::bold;
        if !self.has_starved_tasks() {
            return Vec::new();
        }
        let answered = self
            .waiting_recorded_at
            .map(|answered| {
                format!(
                    " (the owner answered {})",
                    answered.format("%Y-%m-%d %H:%M:%SZ")
                )
            })
            .unwrap_or_default();
        let mut lines = vec![format!(
            "{} {} admissible and {} excluded backlog task(s) were never started at the last pass{answered}",
            bold("Still waiting:"),
            self.queued.unwrap_or(0),
            self.excluded_total,
        )];
        for task in &self.deferred {
            lines.push(format!("  {}", waiting_line(task, "lock conflict")));
        }
        for task in &self.excluded {
            lines.push(format!("  {}", waiting_line(task, "excluded")));
        }
        let listed = self.excluded.len() as u64;
        if self.excluded_total > listed {
            lines.push(format!(
                "  ... and {} more excluded",
                self.excluded_total - listed
            ));
        }
        if let Some(line) = self.idle_line() {
            lines.push(line);
        }
        lines
    }

    fn idle_line(&self) -> Option<String> {
        let kept_off: u64 = self.by_reason.values().sum();
        if self.idle_passes < IDLE_SUMMARY_PASSES || kept_off == 0 {
            return None;
        }
        let mut by_count = self.by_reason.iter().collect::<Vec<_>>();
        by_count.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let causes = by_count
            .into_iter()
            .map(|(reason, count)| format!("{count} {}", kept_off_cause(reason)))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "  idle: {kept_off} backlog task(s) kept off this host for {} consecutive passes ({causes})",
            self.idle_passes,
        ))
    }
}

/// What a pull drain's reason code says about the tasks it counts.
fn kept_off_cause(reason: &str) -> &str {
    match reason {
        "context_lock_conflict" => "footprint holds",
        "dependency_not_done" => "unmet dependencies",
        "host_os_mismatch" => "for another OS",
        "native_os_required" => "needing native evidence from another OS",
        "crew_unavailable" => "needing a crew this host cannot run",
        "owner_hold" => "held on the owner",
        "invalid_candidate" => "invalid",
        other => other,
    }
}

/// The `Approved:` line for an `--approve-proposed` drain, then each held
/// proposed task with the reason it stayed proposed.
fn approval_lines(report: &DrainApprovalReport) -> Vec<String> {
    let closed = if report.closed_total > 0 {
        format!(
            "; {} closed as already fixed ({})",
            report.closed_total,
            report.closed.join(", ")
        )
    } else {
        String::new()
    };
    let mut lines = vec![format!(
        "{} {} proposed task(s) moved to backlog{closed}; {} held{}",
        crate::output::color::bold("Approved:"),
        report.approved_total,
        report.held_total,
        if report.held_by_reason.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                report
                    .held_by_reason
                    .iter()
                    .map(|(reason, count)| format!("{reason}={count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    )];
    for task in &report.held {
        let task = WaitingTask {
            task_id: task.task_id.clone(),
            reason: task.reason.clone(),
            blocked_by: task.blocked_by.clone(),
            detail: task.detail.clone(),
        };
        lines.push(format!("  {}", waiting_line(&task, "held")));
    }
    let listed = report.held.len() as u64;
    if report.held_total > listed {
        lines.push(format!(
            "  ... and {} more held",
            report.held_total - listed
        ));
    }
    lines
}

/// The `Throttled:` line for a drain whose last pass host pressure held.
fn throttle_line(
    throttle: &ResourceThrottle,
    recorded_at: Option<chrono::DateTime<chrono::Utc>>,
) -> String {
    let at = recorded_at
        .map(|at| format!(" (last pass {})", at.format("%Y-%m-%d %H:%M:%SZ")))
        .unwrap_or_default();
    format!(
        "{} {}{at}",
        crate::output::color::bold("Throttled:"),
        throttle.hold_reason()
    )
}

/// The `Throttled:` line for a pull drain, which has no leaf summary of its
/// own: its owner orders the backlog [ORB-13901].
pub(super) fn pass_throttle_line(pass: &DrainAdmissionPass) -> Option<String> {
    pass.resource_throttle
        .as_ref()
        .map(|throttle| throttle_line(throttle, Some(pass.recorded_at)))
}

/// The `Still waiting:` lines for a pull drain, which has no leaf summary of
/// its own: they come from the owner's last answer, as the drain recorded it
/// [ORB-14475].
pub(super) fn pass_waiting_lines(pass: &DrainAdmissionPass) -> Vec<String> {
    waiting_backlog(pass).lines()
}

fn waiting_line(task: &WaitingTask, default_reason: &str) -> String {
    let mut line = format!(
        "Task {}: {}",
        task.task_id,
        task.reason.as_deref().unwrap_or(default_reason)
    );
    if !task.blocked_by.is_empty() {
        line.push_str(&format!(" blocked-by={}", task.blocked_by.join(",")));
    }
    // A host-OS wait names the host it waits for, and a native-OS
    // requirement the tag to add. A local-route review hold names the
    // remedy. A verified-no-diff hold names the pilot's evidence and the
    // commits it cites. Other details (long repair instructions) stay in `--json`.
    if matches!(
        task.reason.as_deref(),
        Some(
            "host_os_mismatch"
                | "native_os_required"
                | "local_route_before_pr"
                | "local_route_before_landing"
                | "crew_unavailable"
                | "owner_hold"
                | "invalid_candidate"
                | "pilot_verified_no_diff"
        )
    ) && let Some(detail) = &task.detail
    {
        line.push_str(&format!(" ({detail})"));
    }
    line
}

/// Summarize a drain's leaves, or `None` for any other run.
///
/// `read_child` resolves a dispatched run id to its record; an unreadable one
/// is counted rather than failing the whole view.
pub(super) fn summarize_drain_leaves(
    run: &JobRun,
    state: Option<&PipelineState>,
    mut read_child: impl FnMut(&str) -> Option<JobRun>,
) -> Option<DrainLeafSummary> {
    if run.job_id != DRAIN_JOB {
        return None;
    }
    let state = state?;
    let mut summary = DrainLeafSummary {
        waiting: last_pass_waiting(state),
        approvals: state.drain_approvals.clone(),
        capacity: state
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
            JobRunState::Success => summary.succeeded += 1,
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
            _ => summary.running += 1,
        }
    }
    if summary.admitted == 0
        && summary.waiting == WaitingBacklog::default()
        && summary.approvals.is_none()
    {
        return None;
    }
    Some(summary)
}

/// What the drain's last admission pass left waiting, from the record it keeps
/// on its own run state.
fn last_pass_waiting(state: &PipelineState) -> WaitingBacklog {
    state
        .drain_last_pass
        .as_ref()
        .map(waiting_backlog)
        .unwrap_or_default()
}

fn waiting_backlog(pass: &DrainAdmissionPass) -> WaitingBacklog {
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
        excluded: tasks(&pass.excluded),
        excluded_total: pass.excluded_total,
        resource_throttle: pass.resource_throttle.clone(),
        recorded_at: Some(pass.recorded_at),
        waiting_recorded_at: pass.waiting_recorded_at,
        by_reason: pass.waiting_by_reason.clone(),
        idle_passes: pass.consecutive_idle_passes,
    }
}
