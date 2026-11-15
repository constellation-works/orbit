//! The `activity.explore_brief_applied` audit record, kept so audit rows
//! written before the explore brief was removed [ORB-22805] still parse.
//! Nothing emits it any more.

use serde::{Deserialize, Serialize};

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
    /// Whether the prompt carried the brief.
    pub delivered: bool,
}
