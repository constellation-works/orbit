//! Per-minute CPU and memory history for auto-concurrency profiles: each
//! delivery run's worker scope, and the host as a whole.
//!
//! CPU is in cores (CPU time over wall time) and memory in non-reclaimable
//! bytes on both sides, so the host's load minus the sum of its runs is the
//! load that is not from tasks.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

/// One minute of one run's worker scope.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResourceMinute {
    pub workspace_id: String,
    pub run_id: String,
    /// Minutes since the worker started sampling.
    pub minute_index: u32,
    /// When that minute began.
    pub minute_started_at: DateTime<Utc>,
    pub task_id: String,
    /// `auto-task:<template>`, `no-diff` or `change`, captured once per
    /// delivery.
    pub work_class: String,
    pub complexity: Option<String>,
    /// A resumed, replayed or retried delivery, whose clock restarted part
    /// way through. Profiles exclude these.
    pub resumed: bool,
    /// When the drain admitted the delivery, so the runs of one delivery can
    /// be aligned on one clock.
    pub delivery_admitted_at: Option<DateTime<Utc>>,
    /// Mean cores used over the minute.
    pub cpu_cores: f64,
    /// Maximum of `memory.current - file + shmem` seen in the minute.
    pub memory_bytes: u64,
    pub samples: u32,
}

/// One wall-clock minute of the host.
#[derive(Debug, Clone, PartialEq)]
pub struct HostResourceMinute {
    /// The minute's start, on a whole UTC minute.
    pub minute_at: DateTime<Utc>,
    /// Mean busy cores over the minute, from `/proc/stat` tick deltas.
    pub cpu_cores: f64,
    pub cpu_count: u32,
    /// Maximum of `MemTotal - MemAvailable` seen in the minute.
    pub memory_bytes: u64,
    pub memory_total_bytes: u64,
    pub samples: u32,
    /// The run whose worker offered the row that won.
    pub recorded_by_run_id: Option<String>,
}

/// The two per-minute tables retention prunes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceMinutesTable {
    /// `run_resource_minutes`, keyed by `minute_started_at`.
    Run,
    /// `host_resource_minutes`, keyed by `minute_at`.
    Host,
}

impl ResourceMinutesTable {
    /// The SQLite table this names.
    pub const fn table_name(self) -> &'static str {
        match self {
            Self::Run => "run_resource_minutes",
            Self::Host => "host_resource_minutes",
        }
    }
}

pub trait ResourceUsageStoreBackend: Send + Sync {
    /// Record one run minute. Writing the same minute again replaces it, so a
    /// sampler that re-flushes a minute never duplicates it.
    fn record_run_resource_minute(&self, minute: &RunResourceMinute) -> Result<(), OrbitError>;

    /// Offer the host's row for one minute. The first offer of a minute wins;
    /// returns whether this one was stored.
    fn offer_host_resource_minute(&self, minute: &HostResourceMinute) -> Result<bool, OrbitError>;

    /// One run's minutes, in minute order.
    fn run_resource_minutes(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Vec<RunResourceMinute>, OrbitError>;

    /// Host minutes at or after `since`, oldest first.
    fn host_resource_minutes(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError>;

    /// Rows of `table` from minutes before `cutoff`, across every workspace.
    fn resource_minutes_older_than(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, OrbitError>;

    /// Delete at most `limit` of the rows [`Self::resource_minutes_older_than`]
    /// counts, in one statement. Returns how many went.
    fn prune_resource_minutes_batch(
        &self,
        table: ResourceMinutesTable,
        cutoff: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, OrbitError>;
}
