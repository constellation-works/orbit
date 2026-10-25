//! The `tags_changed` history entry a task write appends when it changes
//! the task's tags, and the replay that recovers the tags in force at any
//! earlier time.
//!
//! Each entry carries the full previous and new tag lists as a typed
//! [`TaskDecision::TagsChanged`], so the tags before the first recorded
//! change are that change's `previous` list and no creation snapshot is
//! needed. A task whose history holds no change has always carried its
//! current tags. Tag edits made before writes recorded them cannot be
//! reconstructed; replay then reads the current tags for every time.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{TaskDecision, TaskHistoryEntry};

/// History event of a write that changed a task's tags.
pub const TAGS_CHANGED_EVENT: &str = "tags_changed";

/// The tag lists before and after one write that changed them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagsChanged {
    /// The tags the write replaced.
    pub previous: Vec<String>,
    /// The tags the write left on the task.
    pub tags: Vec<String>,
}

/// A history entry carries a decision that is not a tag change, so no
/// legacy text may stand in for it.
#[derive(Debug, thiserror::Error)]
#[error("tags history carries a mismatched decision")]
pub struct TagsChangedError;

impl TagsChanged {
    /// Read the tag change one history entry carries. `Ok(None)` means the
    /// entry is not a `tags_changed` event or carries no decision; a
    /// mismatched decision on a `tags_changed` event is an error and must
    /// never fall through to text.
    pub fn from_entry(entry: &TaskHistoryEntry) -> Result<Option<Self>, TagsChangedError> {
        if entry.event != TAGS_CHANGED_EVENT {
            return Ok(None);
        }
        match &entry.decision {
            Some(TaskDecision::TagsChanged(change)) => Ok(Some(change.clone())),
            Some(_) => Err(TagsChangedError),
            None => Ok(None),
        }
    }
}

/// The tags in force at `at`, replayed from `history` against the task's
/// current tags: the new list of the last recorded change at or before
/// `at`, else the previous list of the first recorded change, else the
/// current tags. An entry whose decision cannot be read is skipped, so a
/// corrupt record degrades to the neighbouring snapshot rather than
/// failing the reader.
pub fn tags_at(history: &[TaskHistoryEntry], current: &[String], at: DateTime<Utc>) -> Vec<String> {
    let mut changes: Vec<(DateTime<Utc>, TagsChanged)> = Vec::new();
    for entry in history {
        if let Ok(Some(change)) = TagsChanged::from_entry(entry) {
            changes.push((entry.at, change));
        }
    }
    changes.sort_by_key(|(changed_at, _)| *changed_at);
    if let Some((_, change)) = changes
        .iter()
        .rev()
        .find(|(changed_at, _)| *changed_at <= at)
    {
        return change.tags.clone();
    }
    match changes.first() {
        Some((_, change)) => change.previous.clone(),
        None => current.to_vec(),
    }
}
