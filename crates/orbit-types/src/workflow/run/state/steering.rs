//! Mid-run steering's snapshot watermark: which task comments the prompts of
//! one run already carried.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Task history event, and the bracketed marker of its run's failure, for a
/// task the promotion gate blocked because operator steering stayed
/// undelivered after the run's steering rounds.
pub const STEERING_UNCONSUMED_EVENT: &str = "steering_unconsumed";

/// The bracketed marker form of [`STEERING_UNCONSUMED_EVENT`].
pub const STEERING_UNCONSUMED_MARKER: &str = "[steering_unconsumed]";

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
    /// none included a comment. A steering block advances it only past the
    /// comments it carried in full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<CommentLogMark>,
    /// Comments past `comment` that a steering block already carried in
    /// full. They stay ahead of the watermark only while an older operator
    /// comment the block had to cut down still awaits delivery.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub delivered: BTreeSet<CommentLogMark>,
    /// Each step's own snapshot, keyed by step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, StepSteeringSnapshot>,
}

impl SteeringWatermark {
    /// Record that `step_id` built a prompt whose newest comment is `newest`,
    /// keeping the later of each recorded mark and `newest`.
    pub fn record(&mut self, step_id: &str, newest: Option<CommentLogMark>, at: DateTime<Utc>) {
        self.comment = self.comment.max(newest);
        let floor = self.comment;
        self.delivered.retain(|mark| Some(*mark) > floor);
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

    /// Record that `step_id` built a prompt whose steering block carried
    /// `delivered` in full, and after which every operator comment up to
    /// `through` has reached one of the run's prompts in full. Delivered
    /// comments past `through` are kept so no later block carries them again.
    pub fn record_delivery(
        &mut self,
        step_id: &str,
        through: Option<CommentLogMark>,
        delivered: &[CommentLogMark],
        at: DateTime<Utc>,
    ) {
        self.record(step_id, through, at);
        let floor = self.comment;
        self.delivered
            .extend(delivered.iter().filter(|mark| Some(**mark) > floor));
    }

    /// Whether the comment at `position` is behind the watermark or was
    /// already carried in full by one of the run's steering blocks.
    pub fn covers(&self, position: u64) -> bool {
        self.comment.is_some_and(|mark| position <= mark.position)
            || self.delivered.iter().any(|mark| mark.position == position)
    }
}

/// One promotion-gate steering round a run admitted for a task
/// (`docs/design/mid-run-steering/2_design.md` §3).
///
/// The host records it before the round's agent step is dispatched again, so
/// the round is spent even when that dispatch fails before any prompt is
/// built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringRound {
    /// When the host admitted the round.
    pub admitted_at: DateTime<Utc>,
    /// The operator comments the gate found undelivered when it admitted the
    /// round, oldest first.
    pub outstanding: Vec<CommentLogMark>,
}
