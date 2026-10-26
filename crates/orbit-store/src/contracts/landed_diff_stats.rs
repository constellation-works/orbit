//! Lines added and deleted per landed delivery: one fact row written on the
//! owner when a delivery lands (or by the bounded backfill), read by the
//! Analytics › Metrics diff trend.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// Lines and files one path class of a landed diff changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffClassCounts {
    pub added: u64,
    pub deleted: u64,
    pub files: u64,
}

/// How a row was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandedDiffSource {
    /// Written when the delivery landed.
    Landing,
    /// Written later by the backfill from a landing receipt.
    Backfill,
}

impl LandedDiffSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Landing => "landing",
            Self::Backfill => "backfill",
        }
    }

    pub(crate) fn from_stored(raw: &str) -> Self {
        if raw == "backfill" {
            Self::Backfill
        } else {
            Self::Landing
        }
    }
}

/// One landed delivery's diff, `base_commit..landed_commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedDiffStats {
    pub workspace_id: String,
    pub task_id: String,
    pub landed_commit: String,
    pub base_commit: String,
    pub job_run_id: Option<String>,
    pub landed_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub source: LandedDiffSource,
    /// The implementing crew and model as resolved on the delivery run.
    pub crew: Option<String>,
    pub model: Option<String>,
    /// The task's complexity and type when it landed.
    pub complexity: Option<String>,
    pub task_type: Option<String>,
    pub code: DiffClassCounts,
    pub tests: DiffClassCounts,
    pub other: DiffClassCounts,
}

pub trait LandedDiffStoreBackend: Send + Sync {
    /// Insert `row` unless the workspace already holds one for its task and
    /// landed commit. `true` when this call wrote it.
    fn record_landed_diff_stats(&self, row: &LandedDiffStats) -> Result<bool, OrbitError>;

    /// The workspace's rows that landed at or after `since` (every row when
    /// `None`), oldest first.
    fn landed_diff_stats_since(
        &self,
        workspace_id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<LandedDiffStats>, OrbitError>;

    /// The tasks that already have a row in the workspace.
    fn landed_diff_task_ids(
        &self,
        workspace_id: &str,
    ) -> Result<std::collections::BTreeSet<String>, OrbitError>;
}
