//! When terminal tasks actually reached their status, for closed-task analytics.
//!
//! The generated index dates a terminal row by `updated_at`, which any later
//! comment, artifact or crew backfill advances. That makes it an upper bound
//! on the transition, never the transition's time: this read uses it only to
//! narrow candidates, then dates each one from its durable status history.
//! [`TerminalEntryMemo`] keeps each clean history read's answer while the
//! task's logs are unchanged, so a repeated read pays a file stamp per
//! candidate instead of a history read.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

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

/// What one clean history read answered for a task: the latest entry into
/// `status`, or `None` when its readable history records none.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedEntry {
    /// History stamp taken before the read.
    stamp: String,
    status: TaskStatus,
    entry_at: Option<DateTime<Utc>>,
}

type RecordedEntries = HashMap<String, HashMap<String, RecordedEntry>>;

/// How a read learns one candidate's latest entry into its status.
enum EntrySource {
    /// An earlier clean read's answer, still vouched for by the stamp.
    Recorded(Option<DateTime<Utc>>),
    /// Read history now, recording the answer under this stamp if any.
    Read(Option<String>),
}

/// Latest status entries of terminal candidates, by workspace and task id,
/// each with the history stamp taken before the history read that found it.
///
/// Any store write to a task's event or comment log changes its stamp
/// (`task_history_stamps`), so while the stamp holds a read would find the
/// same latest entry, or the same absence of one. A reconsidered rejection,
/// a new transition or a comment moves the stamp and the next read dates
/// that task from its history again; so does an index status that differs
/// from the recorded one. Only a clean read is recorded: an
/// unreadable or missing history is read, warned about and left undated on
/// every request. A task the store cannot stamp (a write to settle, a worker
/// reading through its owner) is read every time. The stamp's limits are
/// the store's: an out-of-band in-place edit stays unseen until the logs'
/// next store write.
///
/// The entries live for the process, which for the dashboard is the server:
/// the first read after start dates every candidate from history, later ones
/// only what changed. Bounded at [`TerminalEntryMemo::CAPACITY`] entries;
/// overflowing it starts over, costing one full read.
struct TerminalEntryMemo;

impl TerminalEntryMemo {
    const CAPACITY: usize = 1 << 18;

    fn entries() -> MutexGuard<'static, RecordedEntries> {
        static ENTRIES: OnceLock<Mutex<RecordedEntries>> = OnceLock::new();
        ENTRIES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Where each candidate's entry comes from, in candidate order: the
    /// recorded answer while its stamp and status still hold, else a history
    /// read under its current stamp.
    fn lookup(
        workspace: Option<&str>,
        candidates: &[TaskTerminalTransition],
        mut stamps: BTreeMap<String, String>,
    ) -> Vec<EntrySource> {
        let entries = Self::entries();
        let recorded = workspace.and_then(|workspace| entries.get(workspace));
        candidates
            .iter()
            .map(|task| {
                let stamp = stamps.remove(&task.task_id);
                match (
                    stamp,
                    recorded.and_then(|recorded| recorded.get(&task.task_id)),
                ) {
                    (Some(stamp), Some(entry))
                        if entry.stamp == stamp && entry.status == task.status =>
                    {
                        EntrySource::Recorded(entry.entry_at)
                    }
                    (stamp, _) => EntrySource::Read(stamp),
                }
            })
            .collect()
    }

    fn record(workspace: &str, mut fresh: Vec<(String, RecordedEntry)>) {
        if fresh.is_empty() {
            return;
        }
        let mut entries = Self::entries();
        let held: usize = entries.values().map(HashMap::len).sum();
        if held + fresh.len() > Self::CAPACITY {
            entries.clear();
        }
        // A batch larger than the bound still fits after the clear; anything
        // not retained is read from history again next time.
        fresh.truncate(Self::CAPACITY);
        entries
            .entry(workspace.to_string())
            .or_default()
            .extend(fresh);
    }
}

impl OrbitRuntime {
    /// Terminal tasks dated by their durable status transition; see
    /// [`TerminalTaskDates`]. `dated` holds transitions at or after `since`
    /// (every one for `None`). A task's last write is never older than its
    /// transition, so index rows from `since` onward are a sound superset.
    /// A candidate whose logs are unchanged since an earlier clean read
    /// reuses that read's answer (`TerminalEntryMemo`).
    pub fn task_terminal_dates(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<TerminalTaskDates, OrbitError> {
        let candidates = self.task_terminal_transitions(since)?;
        let workspace = self.workspace_id().ok();
        // Stamps are taken before any read they label.
        let stamps = match &workspace {
            Some(_) => {
                let ids: Vec<String> = candidates.iter().map(|task| task.task_id.clone()).collect();
                self.task_history_stamps(&ids)
            }
            None => BTreeMap::new(),
        };
        let sources = TerminalEntryMemo::lookup(workspace.as_deref(), &candidates, stamps);
        let mut fresh = Vec::new();
        let mut dates = TerminalTaskDates::default();
        for (task, source) in candidates.into_iter().zip(sources) {
            let entry_at = match source {
                EntrySource::Recorded(entry_at) => entry_at,
                EntrySource::Read(stamp) => match self.get_task_history(&task.task_id) {
                    Ok(history) => {
                        let entry_at = latest_status_entry_at(&history, task.status);
                        if let Some(stamp) = stamp {
                            fresh.push((
                                task.task_id.clone(),
                                RecordedEntry {
                                    stamp,
                                    status: task.status,
                                    entry_at,
                                },
                            ));
                        }
                        entry_at
                    }
                    Err(error) => {
                        tracing::warn!(
                            task_id = %task.task_id,
                            %error,
                            "terminal task dates: task history unreadable"
                        );
                        None
                    }
                },
            };
            match entry_at {
                Some(terminal_at) if since.is_none_or(|since| terminal_at >= since) => {
                    dates.dated.push(DatedTerminalTask { terminal_at, task });
                }
                Some(_) => {}
                None => dates.undated.push(task),
            }
        }
        if let Some(workspace) = &workspace {
            TerminalEntryMemo::record(workspace, fresh);
        }
        dates.dated.sort_by_key(|row| row.terminal_at);
        Ok(dates)
    }
}
