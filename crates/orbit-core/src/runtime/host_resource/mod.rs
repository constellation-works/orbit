//! Cheap host observations and shared pressure history. No admission side effects.

use chrono::{DateTime, Utc};
use orbit_config::ResourceThrottleSettings;
use orbit_types::workflow::ResourcePressure;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

mod monitor;
mod platform;
mod pressure;
mod probe;

pub use monitor::HostResourceMonitor;
pub use pressure::{ResourceAdmission, ResourcePressureEvaluator};
pub use probe::{CachedHostResourceProbe, default_host_resource_probe};

/// Native samples are reused for this interval, including failed observations.
pub const RESOURCE_CACHE_TTL: Duration = Duration::from_secs(2);
/// Older samples are unknown and cannot hold admission.
pub const RESOURCE_MAX_AGE: Duration = Duration::from_secs(15);
/// High pressure must be observed across at least this interval to throttle.
pub const RESOURCE_SUSTAIN_WINDOW: Duration = Duration::from_secs(10);
/// Background sampling cadence while admission is consulted. Drains poll every
/// 30-60s, longer than [`RESOURCE_MAX_AGE`], so without it every pass would see
/// an observation gap, reset its hysteresis and never hold.
pub const RESOURCE_TICK_INTERVAL: Duration = Duration::from_secs(5);

/// A disk observation for the filesystem containing `path`.
#[derive(Debug, Clone)]
pub struct DiskSample {
    pub path: PathBuf,
    pub used_percent: Option<f64>,
}

/// Raw probe output. Missing values are unavailable, never zero usage.
#[derive(Debug, Clone)]
pub struct HostResourceSample {
    pub sampled_at: DateTime<Utc>,
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
    pub disks: Vec<DiskSample>,
}

/// Injectable observation boundary; implementations must use bounded, cheap reads.
pub trait HostResourceProbe: Send + Sync {
    fn sample(&self, disk_paths: &[PathBuf]) -> HostResourceSample;
}

/// Resource severity. Unknown is separate from healthy observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSeverity {
    Unknown,
    Ok,
    Elevated,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceReading {
    pub percent: Option<f64>,
    pub severity: ResourceSeverity,
    /// `unavailable`, `stale`, or `invalid` when percent cannot be used.
    pub unknown_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskReading {
    pub path: PathBuf,
    #[serde(flatten)]
    pub reading: ResourceReading,
}

/// Observable verdict for admission consumers; evaluating it does not cancel or start work.
#[derive(Debug, Clone, Serialize)]
pub struct HostResourceStatus {
    pub sampled_at: DateTime<Utc>,
    pub sample_age_seconds: f64,
    pub max_age_seconds: u64,
    pub stale: bool,
    pub cpu: ResourceReading,
    pub memory: ResourceReading,
    /// Highest known usage across all watched paths; absent when none is known.
    pub disk: Option<DiskReading>,
    /// Per-path readings retained for admission's unknown-telemetry diagnostics.
    /// The HTTP projection exposes only the aggregate `disk`.
    #[serde(skip_serializing)]
    pub disks: Vec<DiskReading>,
    pub severity: ResourceSeverity,
    pub throttle: bool,
    pub reason: String,
    /// The held resources behind `throttle`, structured for admission warnings.
    pub pressures: Vec<ResourcePressure>,
    pub thresholds: ResourceThrottleSettings,
}

#[cfg(test)]
mod tests;
