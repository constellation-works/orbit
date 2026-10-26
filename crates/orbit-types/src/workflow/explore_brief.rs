//! What an implement dispatch did with the task-pilot's explore brief: the
//! admission outcome the audit trail records, and the brief it delivered.
//!
//! A brief is a starting map, never authority. Every implement dispatch
//! records one outcome, `absent` included, so a trial can tell "no brief"
//! from "brief dropped".

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What admission decided about a task's stored brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExploreBriefOutcome {
    /// No covered path changed between the brief's revision and the base.
    Fresh,
    /// A few covered files changed; delivered with the drifted paths.
    Drifted,
    /// A brief exists but is not delivered; the reason says why.
    Dropped,
    /// No current brief: none stored, or superseded by a later receipt.
    Absent,
}

/// Why a stored brief was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExploreBriefDropReason {
    /// The task's material changed since the pilot, or could not be
    /// recomputed.
    MaterialChanged,
    /// The brief's revision is not an ancestor of the base, or the diff
    /// between them cannot be read.
    RevisionUnrelated,
    /// Too much of what the brief covers changed since its revision.
    Stale,
}

impl ExploreBriefDropReason {
    /// The audit and display spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MaterialChanged => "material_changed",
            Self::RevisionUnrelated => "revision_unrelated",
            Self::Stale => "stale",
        }
    }
}

/// The `activity.explore_brief_applied` record of one implement dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExploreBriefRecord {
    pub outcome: ExploreBriefOutcome,
    /// Set only for [`ExploreBriefOutcome::Dropped`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ExploreBriefDropReason>,
    /// A short explanation of an `absent` or `dropped` outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// SHA-256 of the stored brief document, when one was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The revision the pilot inspected when it wrote the brief.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_revision: Option<String>,
    /// The base the brief was classified against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// Covered paths that changed between the two revisions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drifted_paths: Vec<String>,
    /// Whether the prompt carried the brief. A `fresh` or `drifted` brief is
    /// not delivered while `workflow.explore_brief.deliver` is off.
    pub delivered: bool,
}

impl ExploreBriefRecord {
    /// A record with no brief to show for it.
    pub fn absent(detail: impl Into<String>) -> Self {
        Self {
            outcome: ExploreBriefOutcome::Absent,
            reason: None,
            detail: Some(detail.into()),
            sha256: None,
            inspection_revision: None,
            base_sha: None,
            drifted_paths: Vec::new(),
            delivered: false,
        }
    }

    /// `fresh`, `drifted`, `dropped: <reason>` or `absent`.
    pub fn label(&self) -> String {
        match (self.outcome, self.reason) {
            (ExploreBriefOutcome::Fresh, _) => "fresh".to_string(),
            (ExploreBriefOutcome::Drifted, _) => "drifted".to_string(),
            (ExploreBriefOutcome::Dropped, Some(reason)) => format!("dropped: {}", reason.as_str()),
            (ExploreBriefOutcome::Dropped, None) => "dropped".to_string(),
            (ExploreBriefOutcome::Absent, _) => "absent".to_string(),
        }
    }

    /// Whether the outcome lets the brief reach the prompt.
    pub fn deliverable(&self) -> bool {
        matches!(
            self.outcome,
            ExploreBriefOutcome::Fresh | ExploreBriefOutcome::Drifted
        )
    }
}

/// A host's answer for one implement dispatch: the audit record, and the
/// task context's `explore_brief` value when the brief is delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct ExploreBriefAdmission {
    pub record: ExploreBriefRecord,
    /// `Some` only for a deliverable outcome with delivery on.
    pub brief: Option<Value>,
}
