//! Cheap host observations and a process-local pressure verdict. No admission side effects.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_config::ResourceThrottleSettings;
use serde::Serialize;

mod platform;

/// Native samples are reused for this interval, including failed observations.
pub const RESOURCE_CACHE_TTL: Duration = Duration::from_secs(2);
/// Older samples are unknown and cannot hold admission.
pub const RESOURCE_MAX_AGE: Duration = Duration::from_secs(15);
/// High pressure must be observed across at least this interval to throttle.
pub const RESOURCE_SUSTAIN_WINDOW: Duration = Duration::from_secs(10);

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
    pub thresholds: ResourceThrottleSettings,
}

#[derive(Default)]
struct PressureState {
    last_sample: Option<DateTime<Utc>>,
    high_since: Option<DateTime<Utc>>,
    held: bool,
}

/// Stateful hysteresis with a deterministic timestamp seam for fixture-driven checks.
#[derive(Default)]
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
        self.states.retain(|key, _| seen.contains(key));
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
            thresholds: settings.clone(),
        }
    }
}

/// Shared probe and evaluator for one runtime or serving host.
pub struct HostResourceMonitor {
    probe: Arc<dyn HostResourceProbe>,
    settings: ResourceThrottleSettings,
    evaluator: Mutex<ResourcePressureEvaluator>,
}

impl HostResourceMonitor {
    pub fn new(probe: Arc<dyn HostResourceProbe>, settings: ResourceThrottleSettings) -> Self {
        Self {
            probe,
            settings,
            evaluator: Mutex::new(ResourcePressureEvaluator::default()),
        }
    }

    pub fn snapshot(&self, paths: &[PathBuf]) -> HostResourceStatus {
        // Serialize sampling and evaluation together: a delayed reader cannot apply an
        // older sample after a newer sample and reset its hysteresis.
        let mut evaluator = self
            .evaluator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        evaluator.evaluate(self.probe.sample(paths), &self.settings, Utc::now())
    }
}
