//! The host admission lock: one serialized read, decide and reserve step over
//! the delivery runs and admission reservations of every workspace on the host.
//!
//! Each workspace drain is its own process, and only the database shows what
//! another drain just admitted. So the load an admission decides against is
//! read inside the same `IMMEDIATE` transaction that records the decision,
//! never from a cache.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use serde_json::Value;

use super::{HostResourceMinute, RunResourceMinute};

/// A delivery run as admission reads it: live, or created since the oldest
/// reservation.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionRun {
    pub workspace_id: String,
    pub run_id: String,
    pub job_id: String,
    /// `pending`, `running` or `retrying`.
    pub live: bool,
    pub created_at: DateTime<Utc>,
    /// When its worker started; `None` while it has not.
    pub started_at: Option<DateTime<Utc>>,
    pub input: Option<Value>,
    /// The run's newest `run_resource_minutes` row: its current measured use.
    pub newest_minute: Option<RunResourceMinute>,
}

/// An admission decided under the host lock whose run may not exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReservation {
    /// The workspace whose drain will create the run.
    pub workspace_id: String,
    pub task_id: String,
    /// The profile lookup key the admitter chose: work class, complexity and
    /// base class (`no-diff` or `change`).
    pub work_class: String,
    pub complexity: Option<String>,
    pub base_class: String,
    pub reserved_at: DateTime<Utc>,
}

/// One reservation an admitter's decision asks to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAdmissionReservation {
    pub workspace_id: String,
    pub task_id: String,
    pub work_class: String,
    pub complexity: Option<String>,
    pub base_class: String,
}

/// What one locked admission step reads.
#[derive(Debug, Clone, Copy)]
pub struct AdmissionLoadQuery<'a> {
    /// Stamped on the reservations the decision writes.
    pub now: DateTime<Utc>,
    /// The job definitions a delivery is made of.
    pub delivery_jobs: &'a [&'a str],
    /// Reservations written before this are deleted, not read.
    pub reservations_expire_before: DateTime<Utc>,
    /// Host minutes starting at or after this, and run minutes of every
    /// workspace starting at or after it, are read for the baseline. `None`
    /// reads neither.
    pub resource_minutes_since: Option<DateTime<Utc>>,
}

/// One `run_resource_minutes` row of any delivery run, as the baseline reads
/// it: what the run used over the part of the minute it covered.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionRunMinute {
    pub workspace_id: String,
    pub run_id: String,
    pub minute_started_at: DateTime<Utc>,
    /// Mean cores over the part of the minute the worker covered.
    pub cpu_cores: f64,
    pub memory_bytes: u64,
    pub samples: u32,
    /// The run's `finished_at`; `None` while it is live or its row is gone.
    pub run_finished_at: Option<DateTime<Utc>>,
}

/// The host's delivery runs and reservations, read inside the lock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdmissionLoad {
    /// Every workspace's live delivery runs, plus its delivery runs created at
    /// or after the oldest reservation, so a caller can tell which
    /// reservations already have their run.
    pub runs: Vec<AdmissionRun>,
    /// Unexpired reservations, oldest first.
    pub reservations: Vec<AdmissionReservation>,
    /// The host's recorded minutes since `resource_minutes_since`, oldest
    /// first.
    pub host_minutes: Vec<HostResourceMinute>,
    /// Every delivery run's minutes since `resource_minutes_since`, live or
    /// finished, oldest first.
    pub run_minutes: Vec<AdmissionRunMinute>,
}

/// The caller's decision: the reservations to write for what it admitted.
pub type AdmissionDecision<'a> =
    &'a mut dyn FnMut(&AdmissionLoad) -> Result<Vec<NewAdmissionReservation>, OrbitError>;

pub trait AdmissionLoadStoreBackend: Send + Sync {
    /// Under the host lock, an `IMMEDIATE` transaction on this database:
    /// delete expired reservations, read the load fresh, call `decide`, write
    /// one reservation stamped `query.now` per entry it returns, and commit.
    /// A task holds one reservation: those written replace any earlier ones
    /// for the same `(workspace, task)`, while entries of one call for the
    /// same task (a pending pull's alternatives) are all kept. Returns the
    /// reservations written. An error from `decide` writes nothing.
    ///
    /// `decide` runs while the database is locked for writing: it must only
    /// compute, and must not call this store, the network or a subprocess.
    fn admit_under_host_lock(
        &self,
        query: &AdmissionLoadQuery<'_>,
        decide: AdmissionDecision<'_>,
    ) -> Result<Vec<AdmissionReservation>, OrbitError> {
        self.admit_under_host_lock_replacing(query, &[], decide)
    }

    /// Remove each `(workspace, task)` reservation key before reading the
    /// forecast, then decide and write replacements in the same host lock.
    /// An error rolls back removals as well as writes. Used to refresh pending
    /// requests and exchange their capacity for the granted task atomically.
    fn admit_under_host_lock_replacing(
        &self,
        query: &AdmissionLoadQuery<'_>,
        remove: &[(&str, &str)],
        decide: AdmissionDecision<'_>,
    ) -> Result<Vec<AdmissionReservation>, OrbitError>;
}
