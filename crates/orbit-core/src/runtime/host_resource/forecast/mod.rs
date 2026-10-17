//! The host load forecast every admitter on the host decides against, and the
//! host admission lock that serializes those decisions.
//!
//! For CPU and memory separately, over minutes `t = 0 … H` from now:
//!
//! ```text
//! F(t) = B + Σ running i: P_i(a_i + t) · alive_i(a_i + t) / alive_i(a_i)
//!          + Σ reservations r: P_r(t) · alive_r(t)
//! ```
//!
//! - **Running deliveries** are every workspace's live delivery runs, grouped
//!   per task. `a_i` is the whole minutes since the delivery was admitted. A
//!   delivery older than its profile's last minute, or with no profile,
//!   contributes its current measured use, held flat.
//! - **Reservations** are admissions decided under the lock whose run does
//!   not exist yet. One stops counting once a delivery run naming its task is
//!   created in its workspace, or [`RESERVATION_TTL_MINUTES`] after it was
//!   written, when dispatch failed.
//! - **Baseline `B`** is the load that is not from tasks, held flat. It is
//!   the host's newest completed minute whose run minutes are all recorded,
//!   minus those run minutes: both are means over the same wall-clock
//!   minute. With no such minute in the last five minutes, it is the host's
//!   measured load now minus the running deliveries' newest measured
//!   minutes, and a delivery with no minute yet counts at its profile's
//!   expectation for its age. Unknown when the host cannot be read now, and
//!   then counted as zero.
//!
//! The running set, reservations and baseline are read inside the lock on
//! every call and never cached: each drain is its own process, and only the
//! database shows what another just admitted. The completed minutes `B` is
//! aligned to are read just before the lock, as no admitter writes them.
//! Profiles come from the profile cache.
//!
//! [`HostLoadForecast::gap`] is the room gap fit admits into, for a local
//! drain's candidates and a pull drain's kinds of work alike: a candidate
//! fits only if, at every minute of its horizon, the forecast plus its
//! profile stays under `admit_cpu_percent` of the host's cores and
//! `admit_memory_percent` of its memory. While the forecast holds no running
//! delivery or reservation, one that does not fit is admitted anyway: nothing
//! on the host would make room by finishing, and the throttle is the
//! backstop.
//! [`HostLoadForecast::accuracy_points`] is what the pass records of it, to
//! check against the host's measured minutes.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::profile::{ProfileKey, ProfileLookup, ProfilePeak, ProfileSet, ProfileSource};

mod build;
mod fit;
mod host_load;
mod record;

pub(crate) use build::build_forecast;
pub use fit::{ForecastGap, GapMiss, GapResource, IdleHostAdmission};
pub(crate) use host_load::{HostLoadProbe, non_task_minute};
pub use record::{ACCURACY_OFFSETS, ForecastPoints};

/// Minutes after which a reservation whose run never appeared leaves the
/// forecast.
pub const RESERVATION_TTL_MINUTES: i64 = 5;

/// A pull request's mutually exclusive profile alternatives share this key.
pub(crate) fn pending_pull_reservation_id(request_id: &str) -> String {
    format!("pending:{request_id}")
}

/// The host's measured load, in the units of the task samples.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HostLoad {
    /// Busy cores from `/proc/stat` ticks; `None` when no ticks passed since
    /// the previous reading.
    pub cpu_cores: Option<f64>,
    pub cpu_count: u32,
    /// `MemTotal - MemAvailable`.
    pub memory_bytes: u64,
    pub memory_total_bytes: u64,
}

/// One resource's forecast.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForecastSeries {
    /// `B`: load that is not from deliveries, held flat. `None` when the host
    /// could not be measured; the series then counts it as zero.
    pub baseline: Option<f64>,
    /// `F(t)` for `t = 0 … H`: cores for CPU, bytes for memory.
    pub minutes: Vec<f64>,
    /// The highest `F(t)` and its minute; the first one on a tie.
    pub peak: ProfilePeak<f64>,
}

/// A delivery running on the host.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunningDelivery {
    pub workspace_id: String,
    pub task_id: String,
    /// Its live runs.
    pub run_ids: Vec<String>,
    pub work_class: String,
    pub complexity: Option<String>,
    pub admitted_at: DateTime<Utc>,
    /// `a_i`: whole minutes since admission.
    pub age_minutes: u32,
    pub profile_source: ProfileSource,
    pub profile_key: Option<ProfileKey>,
    /// The sum of its live runs' newest measured minutes.
    pub current_cpu_cores: f64,
    pub current_memory_bytes: u64,
    /// Older than its profile's last minute, or without a profile: it
    /// contributes its current use, held flat.
    pub held_flat: bool,
}

/// An admission reserved under the lock whose run does not exist yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReservedAdmission {
    pub workspace_id: String,
    pub task_id: String,
    pub work_class: String,
    pub complexity: Option<String>,
    pub reserved_at: DateTime<Utc>,
    pub profile_source: ProfileSource,
    pub profile_key: Option<ProfileKey>,
}

/// The host's expected CPU and memory over the next `H` minutes.
#[derive(Debug, Clone, Serialize)]
pub struct HostLoadForecast {
    pub at: DateTime<Utc>,
    /// `H`: the profile horizon cap, so it covers any candidate's horizon.
    pub horizon_minutes: u32,
    /// The host's measured load now; `None` when it cannot be read.
    pub host: Option<HostLoad>,
    /// The completed host minute the baselines were taken from; `None` when
    /// they came from the host's load now.
    pub baseline_minute: Option<DateTime<Utc>>,
    pub running: Vec<RunningDelivery>,
    pub reservations: Vec<ReservedAdmission>,
    pub cpu: ForecastSeries,
    pub memory: ForecastSeries,
    /// The profiles the forecast used, for the caller's candidate lookups.
    #[serde(skip)]
    pub profiles: Arc<ProfileSet>,
}

/// A task the caller admitted, to reserve under the lock. The lookup's
/// workspace is the one whose drain will create the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReservationRequest {
    pub task_id: String,
    pub lookup: ProfileLookup,
}

/// A caller's decision against a forecast: its own outcome, and the tasks it
/// admitted.
#[derive(Debug, Clone)]
pub struct ForecastDecision<T> {
    pub outcome: T,
    pub admitted: Vec<AdmissionReservationRequest>,
}
