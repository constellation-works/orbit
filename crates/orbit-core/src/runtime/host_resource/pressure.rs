//! Stateful resource pressure and its admission projection.

use chrono::{DateTime, Utc};
use orbit_config::ResourceThrottleSettings;
use orbit_types::workflow::{ResourcePressure, ResourceThrottle};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{
    DiskReading, HostResourceSample, HostResourceStatus, RESOURCE_MAX_AGE, RESOURCE_SUSTAIN_WINDOW,
    ResourceReading, ResourceSeverity,
};

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
        let disk = disks
            .iter()
            .filter(|disk| disk.reading.percent.is_some())
            .max_by(|a, b| {
                a.reading
                    .percent
                    .unwrap_or_default()
                    .total_cmp(&b.reading.percent.unwrap_or_default())
            })
            .cloned();
        // Evaluate every path above, but report just the worst held disk. Its
        // hold may outlast a newer, higher reading that has not sustained high
        // pressure yet, so choose from held pressures, not the display aggregate.
        let worst_disk = pressures
            .iter()
            .filter(|pressure| pressure.resource.starts_with("disk "))
            .max_by(|a, b| a.percent.total_cmp(&b.percent))
            .cloned();
        pressures.retain(|pressure| !pressure.resource.starts_with("disk "));
        pressures.extend(worst_disk);
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
        let throttle = !pressures.is_empty();
        let reason = if throttle {
            pressures
                .iter()
                .map(|pressure| {
                    format!(
                        "{} {:.1}% (high {}%, resume below {}%, high since {})",
                        pressure.resource,
                        pressure.percent,
                        pressure.high_percent,
                        pressure.resume_percent,
                        pressure.since.to_rfc3339()
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
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
            disk,
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
    pub(super) fn from_status(status: &HostResourceStatus) -> Self {
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
