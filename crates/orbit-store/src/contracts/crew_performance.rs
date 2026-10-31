//! The per-workspace crew-performance snapshot: the implementer by-type
//! cell matrix persisted by the hourly refresh so a crew draw never runs
//! the scorecard scan. One snapshot per workspace; writing replaces the
//! previous one atomically.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;

/// A snapshot older than this is `stale` and unusable.
pub const CREW_PERFORMANCE_SNAPSHOT_MAX_AGE_HOURS: i64 = 24;

/// One `(crew, model, tier, type)` cell of a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewPerformanceCell {
    pub crew: String,
    pub model: String,
    pub tier: String,
    pub task_type: String,
    /// Tasks behind the cell in the evidence window.
    pub n: u64,
    /// The cell's raw score against its fleet expectation.
    pub raw: f64,
    /// The crew's shrunk tier mean the cell was pulled toward.
    pub crew_mean: f64,
    /// The shrunk cell score a draw reads.
    pub score: f64,
}

/// One workspace's persisted snapshot and the metadata it was built with.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewPerformanceSnapshot {
    pub workspace_id: String,
    pub computed_at: DateTime<Utc>,
    /// Evidence window in days (`workflow.crew_performance_window_days`).
    pub window_days: u32,
    /// Hash of the `workflow.crew_performance_types` list it was built with.
    pub types_hash: String,
    /// The type list it was built with, as the writer serialized it (JSON).
    pub types_json: String,
    pub cells: Vec<CrewPerformanceCell>,
}

/// Why a snapshot read is unusable. Phase 3 treats every reason as factor 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrewPerformanceSnapshotUnusable {
    /// No snapshot was ever written for the workspace.
    Missing,
    /// `computed_at` is older than 24 hours.
    Stale,
    /// The snapshot's type-list hash differs from the current config's.
    TypeListChanged,
}

impl CrewPerformanceSnapshotUnusable {
    /// The reason's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Stale => "stale",
            Self::TypeListChanged => "type_list_changed",
        }
    }
}

/// Judge a stored snapshot for a reader at `now` whose current type list
/// hashes to `current_types_hash`: the snapshot when fresh and built from
/// that list, else the typed reason it is unusable. Staleness is checked
/// before the type list, so an old snapshot reads `stale` whatever list it
/// was built from.
pub fn usable_crew_performance_snapshot(
    snapshot: Option<CrewPerformanceSnapshot>,
    now: DateTime<Utc>,
    current_types_hash: &str,
) -> Result<CrewPerformanceSnapshot, CrewPerformanceSnapshotUnusable> {
    let Some(snapshot) = snapshot else {
        return Err(CrewPerformanceSnapshotUnusable::Missing);
    };
    if now - snapshot.computed_at > Duration::hours(CREW_PERFORMANCE_SNAPSHOT_MAX_AGE_HOURS) {
        return Err(CrewPerformanceSnapshotUnusable::Stale);
    }
    if snapshot.types_hash != current_types_hash {
        return Err(CrewPerformanceSnapshotUnusable::TypeListChanged);
    }
    Ok(snapshot)
}

pub trait CrewPerformanceStoreBackend: Send + Sync {
    /// Replace the workspace's snapshot with `snapshot` in one transaction:
    /// a concurrent reader sees the whole old snapshot or the whole new
    /// one, never a mix.
    fn replace_crew_performance_snapshot(
        &self,
        snapshot: &CrewPerformanceSnapshot,
    ) -> Result<(), OrbitError>;

    /// The workspace's stored snapshot, whatever its age or type list.
    fn crew_performance_snapshot(
        &self,
        workspace_id: &str,
    ) -> Result<Option<CrewPerformanceSnapshot>, OrbitError>;

    /// The workspace's snapshot when usable at `now` under
    /// `current_types_hash`, else the typed reason it is not.
    fn usable_crew_performance_snapshot(
        &self,
        workspace_id: &str,
        now: DateTime<Utc>,
        current_types_hash: &str,
    ) -> Result<Result<CrewPerformanceSnapshot, CrewPerformanceSnapshotUnusable>, OrbitError> {
        Ok(usable_crew_performance_snapshot(
            self.crew_performance_snapshot(workspace_id)?,
            now,
            current_types_hash,
        ))
    }
}
