//! Cheap host observations and shared pressure history. No admission side effects.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_config::ResourceThrottleSettings;
use orbit_types::workflow::{ResourcePressure, ResourceThrottle};
use serde::{Deserialize, Serialize};

use orbit_common::fs::io::{
    FileLockOptions, atomic_write_text_volatile, open_read_only_no_follow,
    with_exclusive_file_lock_options,
};

mod platform;

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
/// Background sampling stops once admission has not been consulted this long.
const RESOURCE_TICK_IDLE: Duration = Duration::from_secs(300);

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

/// Single-flight, bounded (one path set) cache, also usable around an injected probe.
pub struct CachedHostResourceProbe {
    source: Arc<dyn HostResourceProbe>,
    cached: Mutex<Option<(Instant, Vec<PathBuf>, HostResourceSample)>>,
}

impl CachedHostResourceProbe {
    pub fn new(source: Arc<dyn HostResourceProbe>) -> Self {
        Self {
            source,
            cached: Mutex::new(None),
        }
    }
}

impl HostResourceProbe for CachedHostResourceProbe {
    fn sample(&self, disk_paths: &[PathBuf]) -> HostResourceSample {
        let mut guard = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, paths, sample)) = &*guard
            && at.elapsed() < RESOURCE_CACHE_TTL
            && paths == disk_paths
        {
            return sample.clone();
        }
        let sample = self.source.sample(disk_paths);
        *guard = Some((Instant::now(), disk_paths.to_vec(), sample.clone()));
        sample
    }
}

/// The process shares its native probe so runtime clones and callers reuse samples.
pub fn default_host_resource_probe() -> Arc<dyn HostResourceProbe> {
    static PROBE: OnceLock<Arc<platform::NativeProbe>> = OnceLock::new();
    PROBE
        .get_or_init(|| Arc::new(platform::NativeProbe::default()))
        .clone()
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
    pub disks: Vec<DiskReading>,
    pub severity: ResourceSeverity,
    pub throttle: bool,
    pub reason: String,
    /// The held resources behind `throttle`, structured for admission warnings.
    pub pressures: Vec<ResourcePressure>,
    pub thresholds: ResourceThrottleSettings,
}

#[derive(Default, Serialize, Deserialize)]
struct PressureState {
    last_sample: Option<DateTime<Utc>>,
    high_since: Option<DateTime<Utc>>,
    held: bool,
    high_percent: u8,
    resume_percent: u8,
}

/// Stateful hysteresis with a deterministic timestamp seam for fixture-driven checks.
#[derive(Default, Serialize, Deserialize)]
pub struct ResourcePressureEvaluator {
    states: BTreeMap<String, PressureState>,
}

impl ResourcePressureEvaluator {
    pub fn evaluate(
        &mut self,
        sample: HostResourceSample,
        settings: &ResourceThrottleSettings,
        now: DateTime<Utc>,
    ) -> HostResourceStatus {
        let age = now.signed_duration_since(sample.sampled_at);
        let stale =
            age.num_milliseconds() < 0 || age.to_std().map_or(true, |age| age > RESOURCE_MAX_AGE);
        let mut reasons = Vec::new();
        let mut pressures = Vec::new();
        let mut seen = Vec::new();
        let mut reading = |key: String, value: Option<f64>, high: u8, resume: u8, cpu: bool| {
            seen.push(key.clone());
            let unknown_reason = if stale {
                Some("stale")
            } else {
                match value {
                    None => Some("unavailable"),
                    Some(v) if !v.is_finite() || v < 0.0 || (!cpu && v > 100.0) => Some("invalid"),
                    _ => None,
                }
            };
            let state = self.states.entry(key.clone()).or_default();
            // History from another workspace is evidence only for the same thresholds.
            if state.high_percent != high || state.resume_percent != resume {
                *state = PressureState::default();
            }
            if unknown_reason.is_some() {
                *state = PressureState::default();
                return ResourceReading {
                    percent: None,
                    severity: ResourceSeverity::Unknown,
                    unknown_reason,
                };
            }
            let value = value.unwrap_or_default();
            // A long observation gap or time reversal does not prove sustained pressure.
            if state.last_sample.is_some_and(|previous| {
                sample.sampled_at < previous
                    || sample
                        .sampled_at
                        .signed_duration_since(previous)
                        .to_std()
                        .map_or(true, |gap| gap > RESOURCE_MAX_AGE)
            }) {
                *state = PressureState::default();
            }
            state.last_sample = Some(sample.sampled_at);
            state.high_percent = high;
            state.resume_percent = resume;
            let severity = if value >= f64::from(high) {
                ResourceSeverity::Critical
            } else if value >= f64::from(resume) {
                ResourceSeverity::Elevated
            } else {
                ResourceSeverity::Ok
            };
            if !settings.enabled || value < f64::from(resume) {
                state.high_since = None;
                state.held = false;
            } else if value >= f64::from(high) {
                let since = *state.high_since.get_or_insert(sample.sampled_at);
                if sample
                    .sampled_at
                    .signed_duration_since(since)
                    .to_std()
                    .is_ok_and(|duration| duration >= RESOURCE_SUSTAIN_WINDOW)
                {
                    state.held = true;
                }
            } else if !state.held {
                state.high_since = None;
            }
            if state.held {
                reasons.push(format!(
                    "{key} {value:.1}% (high {high}%, resume below {resume}%, high since {})",
                    state
                        .high_since
                        .map_or_else(String::new, |since| since.to_rfc3339())
                ));
                pressures.push(ResourcePressure {
                    resource: key.clone(),
                    percent: value,
                    high_percent: high,
                    resume_percent: resume,
                    since: state.high_since.unwrap_or(sample.sampled_at),
                });
            }
            ResourceReading {
                percent: Some(value),
                severity,
                unknown_reason: None,
            }
        };
        let cpu = reading(
            "cpu".into(),
            sample.cpu_percent,
            settings.cpu_high_percent,
            settings.cpu_resume_percent,
            true,
        );
        let memory = reading(
            "memory".into(),
            sample.memory_percent,
            settings.memory_high_percent,
            settings.memory_resume_percent,
            false,
        );
        let disks: Vec<_> = sample
            .disks
            .into_iter()
            .map(|disk| DiskReading {
                reading: reading(
                    format!("disk {}", disk.path.display()),
                    disk.used_percent,
                    settings.disk_high_percent,
                    settings.disk_resume_percent,
                    false,
                ),
                path: disk.path,
            })
            .collect();
        // Keep recent disk history for other workspaces sharing the host root.
        // It is never included in this verdict unless this probe observes it.
        self.states.retain(|key, state| {
            seen.contains(key)
                || state.last_sample.is_some_and(|at| {
                    now.signed_duration_since(at)
                        .to_std()
                        .is_ok_and(|age| age <= RESOURCE_MAX_AGE)
                })
        });
        let severities: Vec<_> = [cpu.severity, memory.severity]
            .into_iter()
            .chain(disks.iter().map(|disk| disk.reading.severity))
            .collect();
        let severity = if severities.contains(&ResourceSeverity::Critical) {
            ResourceSeverity::Critical
        } else if severities.contains(&ResourceSeverity::Elevated) {
            ResourceSeverity::Elevated
        } else if severities.contains(&ResourceSeverity::Unknown) {
            ResourceSeverity::Unknown
        } else {
            ResourceSeverity::Ok
        };
        let throttle = !reasons.is_empty();
        let reason = if throttle {
            reasons.join("; ")
        } else if !settings.enabled {
            "Resource throttle disabled".into()
        } else if severities.contains(&ResourceSeverity::Unknown) {
            "Unknown readings fail open; no sustained known pressure".into()
        } else {
            "No sustained high pressure".into()
        };
        HostResourceStatus {
            sampled_at: sample.sampled_at,
            sample_age_seconds: age.num_milliseconds().max(0) as f64 / 1000.0,
            max_age_seconds: RESOURCE_MAX_AGE.as_secs(),
            stale,
            cpu,
            memory,
            disks,
            severity,
            throttle,
            reason,
            pressures,
            thresholds: settings.clone(),
        }
    }
}

/// What host pressure means for starting new work [ORB-13901].
///
/// Admission consumers stop starting tasks while `throttle` is set; nothing
/// here touches work that is already running. Unknown or stale readings never
/// throttle: they are listed in `unknown` and admission proceeds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ResourceAdmission {
    pub throttle: Option<ResourceThrottle>,
    /// Readings that could not be used, e.g. `cpu unavailable`.
    pub unknown: Vec<String>,
}

impl ResourceAdmission {
    fn from_status(status: &HostResourceStatus) -> Self {
        let mut unknown = Vec::new();
        let mut note = |resource: String, reading: &ResourceReading| {
            if let Some(reason) = reading.unknown_reason {
                unknown.push(format!("{resource} {reason}"));
            }
        };
        note("cpu".into(), &status.cpu);
        note("memory".into(), &status.memory);
        for disk in &status.disks {
            note(format!("disk {}", disk.path.display()), &disk.reading);
        }
        Self {
            throttle: status.throttle.then(|| ResourceThrottle {
                resources: status.pressures.clone(),
            }),
            unknown,
        }
    }
}

/// Shared probe and evaluator for one runtime or serving host.
pub struct HostResourceMonitor {
    probe: Arc<dyn HostResourceProbe>,
    settings: ResourceThrottleSettings,
    evaluator: Mutex<ResourcePressureEvaluator>,
    history_path: Option<PathBuf>,
    /// Whether admission checks keep the evaluator fed between drain passes.
    /// Off for injected probes, whose fixtures drive every sample themselves.
    background: bool,
    /// Last admission consult, read by the background sampler to retire.
    demand: Mutex<Option<Instant>>,
    sampling: AtomicBool,
    /// The last reported admission, so each transition is logged once.
    reported: Mutex<ResourceAdmission>,
}

impl HostResourceMonitor {
    pub fn new(probe: Arc<dyn HostResourceProbe>, settings: ResourceThrottleSettings) -> Self {
        Self {
            probe,
            settings,
            evaluator: Mutex::new(ResourcePressureEvaluator::default()),
            history_path: None,
            background: false,
            demand: Mutex::new(None),
            sampling: AtomicBool::new(false),
            reported: Mutex::new(ResourceAdmission::default()),
        }
    }

    /// Share bounded, freshness-checked hysteresis across processes on this host.
    pub(crate) fn with_shared_history(mut self, global_root: &std::path::Path) -> Self {
        self.history_path = Some(global_root.join("cache/host-resource-pressure.json"));
        self
    }

    /// Keep sampling in the background while admission is being consulted.
    #[must_use]
    pub fn sampled_in_background(mut self) -> Self {
        self.background = true;
        self
    }

    /// The admission verdict for `paths` [ORB-13901]. Disabled settings return
    /// an empty verdict without sampling. Throttle and unknown-telemetry
    /// transitions are logged once each.
    pub fn admission(self: &Arc<Self>, paths: &[PathBuf]) -> ResourceAdmission {
        if !self.settings.enabled {
            return ResourceAdmission::default();
        }
        self.keep_sampling(paths);
        let admission = ResourceAdmission::from_status(&self.snapshot(paths));
        self.report_transition(&admission);
        admission
    }

    fn report_transition(&self, admission: &ResourceAdmission) {
        let mut reported = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
        match (&reported.throttle, &admission.throttle) {
            (None, Some(throttle)) => tracing::warn!(
                target: "orbit.core.host_resource",
                "admissions throttled: {}",
                throttle.describe()
            ),
            (Some(_), None) => tracing::info!(
                target: "orbit.core.host_resource",
                "admissions resumed: host resources are back below their resume marks"
            ),
            _ => {}
        }
        if reported.unknown.is_empty() && !admission.unknown.is_empty() {
            tracing::warn!(
                target: "orbit.core.host_resource",
                "resource telemetry unknown ({}); admission fails open",
                admission.unknown.join(", ")
            );
        } else if !reported.unknown.is_empty() && admission.unknown.is_empty() {
            tracing::info!(
                target: "orbit.core.host_resource",
                "resource telemetry available again"
            );
        }
        *reported = admission.clone();
    }

    fn keep_sampling(self: &Arc<Self>, paths: &[PathBuf]) {
        if !self.background {
            return;
        }
        *self.demand.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        if self.sampling.swap(true, Ordering::AcqRel) {
            return;
        }
        let monitor: Weak<Self> = Arc::downgrade(self);
        let paths = paths.to_vec();
        let spawned = std::thread::Builder::new()
            .name("orbit-host-resource".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(RESOURCE_TICK_INTERVAL);
                    let Some(monitor) = monitor.upgrade() else {
                        return;
                    };
                    let idle = monitor
                        .demand
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_none_or(|at| at.elapsed() > RESOURCE_TICK_IDLE);
                    if idle {
                        monitor.sampling.store(false, Ordering::Release);
                        return;
                    }
                    monitor.snapshot(&paths);
                }
            });
        if let Err(error) = spawned {
            self.sampling.store(false, Ordering::Release);
            tracing::warn!(
                target: "orbit.core.host_resource",
                %error,
                "background resource sampling unavailable; pressure is evaluated per admission pass"
            );
        }
    }

    pub fn snapshot(&self, paths: &[PathBuf]) -> HostResourceStatus {
        // Serialize sampling and evaluation together: a delayed reader cannot apply an
        // older sample after a newer sample and reset its hysteresis.
        let mut evaluator = self
            .evaluator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.settings.enabled
            && let Some(path) = &self.history_path
        {
            // Serialize read/sample/evaluate/write across processes too. The bounded
            // lock prevents resource telemetry from stalling a dispatch indefinitely.
            let result = with_exclusive_file_lock_options(
                path,
                "host resource pressure",
                FileLockOptions {
                    timeout: Duration::from_millis(100),
                    warn_after: Duration::from_millis(100),
                },
                || -> io::Result<HostResourceStatus> {
                    match read_history(path) {
                        Ok(history) => *evaluator = history,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::debug!(%error, "shared resource history unavailable");
                        }
                    }
                    let status =
                        evaluator.evaluate(self.probe.sample(paths), &self.settings, Utc::now());
                    let history = PressureHistory {
                        schema_version: 1,
                        evaluator: &evaluator,
                    };
                    let encoded = serde_json::to_string(&history).map_err(io::Error::other)?;
                    if let Err(error) = atomic_write_text_volatile(path, &encoded) {
                        tracing::debug!(%error, "shared resource history could not be saved");
                    }
                    Ok(status)
                },
            );
            match result {
                Ok(status) => return status,
                Err(error) => {
                    tracing::debug!(%error, "using local resource history");
                }
            }
        }
        evaluator.evaluate(self.probe.sample(paths), &self.settings, Utc::now())
    }
}

#[derive(Serialize)]
struct PressureHistory<'a> {
    schema_version: u8,
    evaluator: &'a ResourcePressureEvaluator,
}

fn read_history(path: &std::path::Path) -> io::Result<ResourcePressureEvaluator> {
    #[derive(Deserialize)]
    struct History {
        schema_version: u8,
        evaluator: ResourcePressureEvaluator,
    }
    let file = open_read_only_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("resource history is not a regular file"));
    }
    let mut encoded = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut encoded)?;
    if encoded.len() > 64 * 1024 {
        return Err(io::Error::other("resource history exceeds size limit"));
    }
    let history: History = serde_json::from_str(&encoded).map_err(io::Error::other)?;
    if history.schema_version != 1 {
        return Err(io::Error::other("unsupported resource history version"));
    }
    Ok(history.evaluator)
}
