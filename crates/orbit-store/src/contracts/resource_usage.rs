//! Per-minute CPU and memory history for auto-concurrency profiles: each
//! delivery run's worker, and the host as a whole. Beside it, each
//! admission pass's forecast, to check against the host's measured minutes.
//!
//! CPU is in cores (CPU time over wall time) and memory in non-reclaimable
//! bytes on both sides, so the host's load minus the sum of its runs is the
//! load that is not from tasks.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

/// CPU accounting from the Linux cgroup microsecond counter.
pub const CPU_ACCOUNTING_LINUX: &str = "linux-cgroup-v1";
/// CPU accounting from macOS own Mach ticks converted with the host timebase.
pub const CPU_ACCOUNTING_MACOS: &str = "macos-mach-ns-v1";

/// One minute of one run's worker.
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
    /// Counter provenance. `None` preserves an older writer's raw evidence;
    /// legacy minutes are eligible for profiles only on Linux.
    pub cpu_accounting: Option<String>,
    /// Maximum non-reclaimable bytes seen in the minute. Linux reads
    /// `memory.current - file + shmem`. macOS sums each process's
    /// `ri_phys_footprint`.
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

/// One minute of a delivery run's load, as [`RunResourceMinute`] stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceMinuteLoad {
    pub minute_started_at: DateTime<Utc>,
    pub cpu_cores: f64,
    /// Counter provenance, unchanged from the stored raw minute.
    pub cpu_accounting: Option<String>,
    pub memory_bytes: u64,
}

/// One sampled run of a delivery, with its lifecycle from `job_runs`, for
/// building profiles.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRunResources {
    pub workspace_id: String,
    pub run_id: String,
    /// `None` when the run's `job_runs` row is gone.
    pub job_id: Option<String>,
    pub task_id: String,
    pub work_class: String,
    pub complexity: Option<String>,
    /// Any of the run's minutes was marked resumed.
    pub resumed: bool,
    /// The delivery's admission time, shared by every run of one delivery.
    pub delivery_admitted_at: DateTime<Utc>,
    /// When the run finished; `None` while it is live or its row is gone.
    pub finished_at: Option<DateTime<Utc>>,
    /// The run's minutes, oldest first.
    pub minutes: Vec<ResourceMinuteLoad>,
}

/// A cheap probe of the sampled runs that have finished: it changes whenever
/// one more finishes, or retention prunes one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryRunsFreshness {
    pub newest_finished_at: Option<DateTime<Utc>>,
    pub finished_runs: u64,
}

/// One minute of an admission pass's forecast, minutes after the pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdmissionForecastPoint {
    pub offset_minutes: u32,
    pub cpu_cores: f64,
    pub memory_bytes: u64,
}

/// One drain admission pass's forecast and decisions.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionForecastRecord {
    pub workspace_id: String,
    /// The drain's run id.
    pub run_id: String,
    pub forecast_at: DateTime<Utc>,
    /// `shadow` or `enforce`.
    pub mode: String,
    /// The host's size when the forecast was built; `None` when it could not
    /// be read.
    pub cpu_count: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    /// The host's forecast load at each recorded offset, counting what the
    /// pass admitted.
    pub points: Vec<AdmissionForecastPoint>,
    /// The tasks the pass admitted.
    pub admitted: Vec<String>,
    /// The tasks gap fit admitted: under `shadow`, what `enforce` would have.
    pub fit_admitted: Vec<String>,
    /// The tasks gap fit kept out.
    pub fit_skipped: Vec<String>,
    /// A pull request's decision; absent for local admission passes.
    pub pull: Option<PullForecastDecision>,
}

/// The request decision made against a follower's forecast, including
/// requests that returned no claim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullForecastDecision {
    /// Whether the follower sent the request to the owner.
    pub sent: bool,
    /// Whether enforce would hold the request because no kind of work fits.
    pub forecast_no_headroom: bool,
}

/// The resource history tables retention prunes on their 30-day window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceMinutesTable {
    /// `run_resource_minutes`, keyed by `minute_started_at`.
    Run,
    /// `host_resource_minutes`, keyed by `minute_at`.
    Host,
    /// `admission_forecasts`, keyed by `forecast_at`.
    AdmissionForecast,
}

impl ResourceMinutesTable {
    /// The SQLite table this names.
    pub const fn table_name(self) -> &'static str {
        match self {
            Self::Run => "run_resource_minutes",
            Self::Host => "host_resource_minutes",
            Self::AdmissionForecast => "admission_forecasts",
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

    /// Every sampled run of a delivery admitted at or after `since`, across
    /// every workspace on the host, with its minutes and `job_runs` lifecycle.
    fn delivery_run_resources(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<DeliveryRunResources>, OrbitError>;

    /// The finished sampled runs on the host, as a cache freshness probe.
    fn delivery_runs_freshness(&self) -> Result<DeliveryRunsFreshness, OrbitError>;

    /// Host minutes at or after `since`, oldest first.
    fn host_resource_minutes(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError>;

    /// Record one admission pass's forecast.
    fn record_admission_forecast(&self, record: &AdmissionForecastRecord)
    -> Result<(), OrbitError>;

    /// Admission forecasts made at or after `since`, across every workspace,
    /// oldest first.
    fn admission_forecasts(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<AdmissionForecastRecord>, OrbitError>;

    /// Rows of `table` from before `cutoff`, across every workspace.
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
