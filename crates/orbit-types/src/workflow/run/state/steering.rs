//! Mid-run steering's snapshot watermark: which task comments the prompts of
//! one run already carried.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One comment's identity in a task's append-only comment log.
///
/// Marks order by `position` first: the log only grows, so a later comment
/// always holds a larger position, even when it was recorded in the same
/// instant as an earlier one. `at` is the comment's own recorded timestamp,
/// never the reader's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommentLogMark {
    /// Zero-based position of the comment in the task's comment log.
    pub position: u64,
    /// When the comment was recorded.
    pub at: DateTime<Utc>,
}

/// The newest comment one step's task-context prompts included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSteeringSnapshot {
    /// Absent while every prompt the step built held no comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<CommentLogMark>,
    /// When the host last built a task-context prompt for the step.
    pub recorded_at: DateTime<Utc>,
}

/// The newest comment of one task that this run's prompts included.
///
/// It belongs to the run, not the task: two runs of one task, such as a run
/// and its resume, hold independent watermarks. Nothing moves it backwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringWatermark {
    /// The newest comment any of the run's prompts included; absent while
    /// none included a comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<CommentLogMark>,
    /// Each step's own snapshot, keyed by step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, StepSteeringSnapshot>,
}

impl SteeringWatermark {
    /// Record that `step_id` built a prompt whose newest comment is `newest`,
    /// keeping the later of each recorded mark and `newest`.
    pub fn record(&mut self, step_id: &str, newest: Option<CommentLogMark>, at: DateTime<Utc>) {
        self.comment = self.comment.max(newest);
        let step = self
            .steps
            .entry(step_id.to_string())
            .or_insert(StepSteeringSnapshot {
                comment: None,
                recorded_at: at,
            });
        step.comment = step.comment.max(newest);
        step.recorded_at = step.recorded_at.max(at);
    }
}
