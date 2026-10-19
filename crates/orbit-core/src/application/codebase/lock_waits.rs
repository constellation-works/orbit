//! Lock wait episodes: how long each task waited for a context lock on a path.
//!
//! No record stores a wait's duration. Every refused `orbit.task.locks.reserve`
//! is kept as a lock denial with the tasks it named, outside audit retention;
//! every grant is in the reservation history, and every status change is in
//! task history; a wait is read off those. It starts at a task's first denial on a path and ends at its
//! admission (its next reservation grant) or at the end of its eligibility
//! (its next status change away from `backlog`). Retries in between are the
//! same wait.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::facts::CodebaseFacts;
use super::sections::Params;

/// How a wait episode ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WaitEnd {
    /// The task was granted its reservation.
    Admitted,
    /// The task left the backlog without one: done, archived, rejected,
    /// blocked or otherwise no longer waiting to be admitted.
    LeftBacklog,
    /// Neither yet: the wait is measured to the scan.
    Waiting,
}

impl WaitEnd {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::LeftBacklog => "left_backlog",
            Self::Waiting => "waiting",
        }
    }
}

/// One task's wait for a lock on one path: from its first denial to its
/// admission or its leaving the backlog. Retries in between add denials, not
/// episodes.
#[derive(Debug, Clone)]
pub(super) struct WaitEpisode {
    pub(super) task_id: String,
    pub(super) started_at: DateTime<Utc>,
    /// When the wait ended; the scan time while it is still waiting.
    pub(super) ended_at: DateTime<Utc>,
    pub(super) end: WaitEnd,
    pub(super) denials: usize,
}

impl WaitEpisode {
    pub(super) fn wait_ms(&self) -> i64 {
        (self.ended_at - self.started_at).num_milliseconds().max(0)
    }
}

/// A row's waits: how many, how many still waiting, and their median,
/// longest and summed durations. `null` when no task waited on the path, as
/// when every denial came from a file-only request.
pub(super) fn wait_summary(waits: &[WaitEpisode]) -> Value {
    if waits.is_empty() {
        return Value::Null;
    }
    let mut durations: Vec<i64> = waits.iter().map(WaitEpisode::wait_ms).collect();
    durations.sort_unstable();
    let middle = durations.len() / 2;
    let median = if durations.len().is_multiple_of(2) {
        (durations[middle - 1] + durations[middle]) / 2
    } else {
        durations[middle]
    };
    json!({
        "episodes": waits.len(),
        "waiting": waits.iter().filter(|wait| wait.end == WaitEnd::Waiting).count(),
        "median_ms": median,
        "max_ms": durations.last().copied().unwrap_or_default(),
        "total_ms": durations.iter().sum::<i64>(),
    })
}

/// Every task's wait episodes, by rolled-up path, that have a denial in the
/// window. Denials are read in time order: the first for a task and path
/// opens an episode, which ends at the task's first reservation grant or
/// status change away from `backlog` at or after it. A denial before that end
/// is a retry of the same wait; one after it opens a new episode.
///
/// A wait is measured whole even when it began before the window, so the
/// figure a row shows does not depend on where the window cuts it.
pub(super) fn wait_episodes(facts: &CodebaseFacts, params: &Params) -> Vec<(String, WaitEpisode)> {
    let left_backlog: BTreeMap<&str, &[DateTime<Utc>]> = facts
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.left_backlog.as_slice()))
        .collect();
    let first_at_or_after = |times: Option<&[DateTime<Utc>]>, at: DateTime<Utc>| {
        times.and_then(|times| times.iter().find(|time| **time >= at).copied())
    };
    let mut denials: Vec<_> = facts
        .lock_denials
        .iter()
        .filter(|denial| !denial.task_ids.is_empty())
        .collect();
    denials.sort_by_key(|denial| denial.at);

    // (task, path) -> (episode, whether a denial of it falls in the window).
    let mut open: BTreeMap<(&str, String), usize> = BTreeMap::new();
    let mut episodes: Vec<(String, WaitEpisode, bool)> = Vec::new();
    for denial in denials {
        let within = params.within(denial.at);
        for task_id in &denial.task_ids {
            for key in params.rollup.keys(&denial.files) {
                if let Some(&index) = open.get(&(task_id.as_str(), key.clone())) {
                    let (_, episode, in_window) = &mut episodes[index];
                    if episode.end == WaitEnd::Waiting || denial.at < episode.ended_at {
                        episode.denials += 1;
                        *in_window |= within;
                        continue;
                    }
                }
                let admitted =
                    first_at_or_after(facts.admissions.get(task_id).map(Vec::as_slice), denial.at);
                let left =
                    first_at_or_after(left_backlog.get(task_id.as_str()).copied(), denial.at);
                let (ended_at, end) = match (admitted, left) {
                    (Some(admitted), Some(left)) if left < admitted => (left, WaitEnd::LeftBacklog),
                    (Some(admitted), _) => (admitted, WaitEnd::Admitted),
                    (None, Some(left)) => (left, WaitEnd::LeftBacklog),
                    (None, None) => (facts.computed_at.max(denial.at), WaitEnd::Waiting),
                };
                open.insert((task_id.as_str(), key.clone()), episodes.len());
                episodes.push((
                    key,
                    WaitEpisode {
                        task_id: task_id.clone(),
                        started_at: denial.at,
                        ended_at,
                        end,
                        denials: 1,
                    },
                    within,
                ));
            }
        }
    }
    episodes
        .into_iter()
        .filter(|(_, _, in_window)| *in_window)
        .map(|(key, episode, _)| (key, episode))
        .collect()
}
