//! What a drain's leaves did, for `orbit run show`.
//!
//! A drain's own state answers "did the coordinator run", not "did the work
//! ship": it dispatches its leaves detached and never observes their outcomes,
//! so a drain whose every leaf failed still finishes as `success`. This
//! summary reads each leaf's own run, plus the drain's last admission pass, so
//! the operator sees admitted / succeeded / failed / still-waiting in one
//! place. It is additive: the drain's state and every existing field are
//! unchanged.

use orbit_core::JobRun;
use orbit_core::application::job::run_error_step;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use super::format::summarize_error_message;

/// The coordinator job whose leaves this summarizes.
pub(super) const DRAIN_JOB: &str = "workspace_auto_pipeline";
/// The per-task job a drain dispatches, one run per admitted task.
const LEAF_JOB: &str = "task_auto_pipeline";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FailedLeaf {
    pub(super) run_id: String,
    pub(super) state: JobRunState,
    pub(super) task_ids: Vec<String>,
    pub(super) error: Option<String>,
}

/// Backlog tasks the drain's last admission pass left unstarted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WaitingBacklog {
    /// Admissible tasks not admitted in the last pass: no slot, or a lock
    /// conflict. `None` when the drain recorded no pass.
    pub(super) queued: Option<u64>,
    /// The subset of `queued` a lock conflict kept out, with the blocking tasks.
    pub(super) deferred: Vec<WaitingTask>,
    /// Backlog tasks the drain could not admit at all, with the reason.
    pub(super) excluded: Vec<WaitingTask>,
    pub(super) excluded_total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WaitingTask {
    pub(super) task_id: String,
    pub(super) reason: Option<String>,
    pub(super) blocked_by: Vec<String>,
}

impl DrainLeafSummary {
    pub(super) fn has_failed_leaves(&self) -> bool {
        !self.failed_leaves.is_empty()
    }

    /// Whether the drain left admissible or excluded work unstarted.
    pub(super) fn has_starved_tasks(&self) -> bool {
        self.waiting.queued.is_some_and(|queued| queued > 0)
            || !self.waiting.deferred.is_empty()
            || self.waiting.excluded_total > 0
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
            })).collect::<Vec<_>>(),
            "waiting": {
                "queued": self.waiting.queued,
                "deferred": tasks(&self.waiting.deferred),
                "excluded": tasks(&self.waiting.excluded),
                "excluded_total": self.waiting.excluded_total,
            },
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
                lines.push(format!(
                    "    retry: `orbit job resume {}` (or move the task back to backlog)",
                    leaf.run_id
                ));
            }
        }
        if self.has_starved_tasks() {
            let queued = self.waiting.queued.unwrap_or(0);
            lines.push(format!(
                "{} {} admissible and {} excluded backlog task(s) were never started at the last pass",
                bold("Still waiting:"),
                queued,
                self.waiting.excluded_total,
            ));
            for task in &self.waiting.deferred {
                lines.push(format!("  {}", waiting_line(task, "lock conflict")));
            }
            for task in &self.waiting.excluded {
                lines.push(format!("  {}", waiting_line(task, "excluded")));
            }
            let listed = self.waiting.excluded.len() as u64;
            if self.waiting.excluded_total > listed {
                lines.push(format!(
                    "  ... and {} more excluded",
                    self.waiting.excluded_total - listed
                ));
            }
        }
        lines
    }
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
                });
            }
            _ => summary.running += 1,
        }
    }
    if summary.admitted == 0 && summary.waiting == WaitingBacklog::default() {
        return None;
    }
    Some(summary)
}

/// The drain's last classification pass, read from its checkpoint by shape:
/// the classifier's output is the only step output carrying `pending_backlog`.
fn last_pass_waiting(state: &PipelineState) -> WaitingBacklog {
    let Some(pass) = state
        .step_outputs
        .values()
        .rev()
        .find(|output| output.get("pending_backlog").is_some())
    else {
        return WaitingBacklog::default();
    };
    let admitted_now = pass
        .get("loose_task_ids")
        .and_then(Value::as_array)
        .map_or(0, |ids| ids.len() as u64);
    let queued = pass
        .get("pending_backlog")
        .and_then(Value::as_u64)
        .map(|pending| pending.saturating_sub(admitted_now));
    let tasks = |key: &str, blocked_key: &str| -> Vec<WaitingTask> {
        pass.get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let task_id = entry.get("task_id").and_then(Value::as_str)?.to_string();
                let blocked_by = entry
                    .get(blocked_key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                Some(WaitingTask {
                    task_id,
                    reason: entry
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    blocked_by,
                })
            })
            .collect()
    };
    let excluded = tasks("excluded_backlog", "blocked_by");
    let excluded_total = pass
        .get("excluded_backlog_total")
        .and_then(Value::as_u64)
        .unwrap_or(excluded.len() as u64);
    WaitingBacklog {
        queued,
        deferred: tasks("deferred_conflicts", "blocking_task_ids"),
        excluded,
        excluded_total,
    }
}
