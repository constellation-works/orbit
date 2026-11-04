//! When terminal tasks actually reached their status, for closed-task analytics.
//!
//! The generated index dates a terminal row by `updated_at`, which any later
//! comment, artifact or crew backfill advances. That makes it an upper bound
//! on the transition, never the transition's time: this read uses it only to
//! narrow candidates, then dates each one from its durable status history.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::TaskTerminalTransition;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};

use crate::OrbitRuntime;

/// A terminal task and the time its history recorded it entering `task.status`.
#[derive(Debug, Clone)]
pub struct DatedTerminalTask {
    /// When the task last entered its current terminal status.
    pub terminal_at: DateTime<Utc>,
    /// The index row. Its `at` is the task's last write, not its completion.
    pub task: TaskTerminalTransition,
}

/// Candidates of one terminal-date read, split by whether history dates them.
#[derive(Debug, Clone, Default)]
pub struct TerminalTaskDates {
    /// Tasks that entered their status at or after the requested bound,
    /// oldest transition first.
    pub dated: Vec<DatedTerminalTask>,
    /// Candidates whose history is unreadable or has no entry into their
    /// status. Their completion time is unknown: `at` only bounds it from
    /// above, so it must not place them in a time bucket.
    pub undated: Vec<TaskTerminalTransition>,
}

/// The latest entry into `status` (creation in `status` included) in `history`.
pub(crate) fn latest_status_entry_at(
    history: &[TaskHistoryEntry],
    status: TaskStatus,
) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(status))
        .map(|entry| entry.at)
}

impl OrbitRuntime {
    /// Terminal tasks dated by their durable status transition; see
    /// [`TerminalTaskDates`]. `dated` holds transitions at or after `since`
    /// (every one for `None`). A task's last write is never older than its
    /// transition, so index rows from `since` onward are a sound superset.
    pub fn task_terminal_dates(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<TerminalTaskDates, OrbitError> {
        let mut dates = TerminalTaskDates::default();
        for task in self.task_terminal_transitions(since)? {
            let history = self.get_task_history(&task.task_id);
            if let Err(error) = &history {
                tracing::warn!(
                    task_id = %task.task_id,
                    %error,
                    "terminal task dates: task history unreadable"
                );
            }
            match history
                .ok()
                .and_then(|history| latest_status_entry_at(&history, task.status))
            {
                Some(terminal_at) if since.is_none_or(|since| terminal_at >= since) => {
                    dates.dated.push(DatedTerminalTask { terminal_at, task });
                }
                Some(_) => {}
                None => dates.undated.push(task),
            }
        }
        dates.dated.sort_by_key(|row| row.terminal_at);
        Ok(dates)
    }
}
