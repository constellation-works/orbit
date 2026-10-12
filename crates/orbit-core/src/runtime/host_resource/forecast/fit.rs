//! Gap fit: whether a candidate's profile fits under the forecast.

use std::fmt;

use super::{HostLoad, HostLoadForecast};
use crate::runtime::host_resource::profile::ResourceProfile;

/// The resource a candidate would push past its threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GapResource {
    Cpu,
    Memory,
}

impl GapResource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }
}

/// Where a candidate first fails to fit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GapMiss {
    pub resource: GapResource,
    /// Minutes from the forecast.
    pub minute: u32,
    /// The forecast at that minute with the candidate, as a share of the
    /// host's cores or memory.
    pub percent: f64,
}

impl GapMiss {
    /// `cpu 94% at +6m`.
    pub fn describe(&self) -> String {
        format!(
            "{} {:.0}% at +{}m",
            self.resource.as_str(),
            self.percent,
            self.minute
        )
    }
}

/// The forecast one admission pass fits candidates into, grown by each one
/// it admits so later candidates count it.
#[derive(Debug, Clone)]
pub struct ForecastGap {
    cpu: Vec<f64>,
    memory: Vec<f64>,
    cpu_total: f64,
    memory_total: f64,
    cpu_limit: f64,
    memory_limit: f64,
}

impl HostLoadForecast {
    /// The room under `admit_cpu_percent` of the host's cores and
    /// `admit_memory_percent` of its memory. `None` when the host could not
    /// be measured: without its size there is no threshold to fit under.
    pub fn gap(&self, admit_cpu_percent: u8, admit_memory_percent: u8) -> Option<ForecastGap> {
        let host = self.host?;
        let cpu_total = f64::from(host.cpu_count);
        let memory_total = host.memory_total_bytes as f64;
        if cpu_total <= 0.0 || memory_total <= 0.0 {
            return None;
        }
        Some(ForecastGap {
            cpu: self.cpu.minutes.clone(),
            memory: self.memory.minutes.clone(),
            cpu_total,
            memory_total,
            cpu_limit: cpu_total * f64::from(admit_cpu_percent) / 100.0,
            memory_limit: memory_total * f64::from(admit_memory_percent) / 100.0,
        })
    }
}

impl ForecastGap {
    /// Whether, for every `t` in `0 ..= H` of `profile`'s horizon,
    /// `F(t) + P.cpu(t) · alive(t)` and the matching memory sum stay at or
    /// under their thresholds; otherwise the first minute that does not,
    /// CPU before memory.
    pub fn fit(&self, profile: &ResourceProfile) -> Result<(), GapMiss> {
        let last = usize::try_from(profile.horizon_minutes)
            .unwrap_or(usize::MAX)
            .min(self.cpu.len().saturating_sub(1));
        for t in 0..=last {
            let (cpu, memory) = profile.series.get(t).map_or((0.0, 0.0), |minute| {
                (
                    minute.cpu_cores * minute.alive,
                    minute.memory_bytes as f64 * minute.alive,
                )
            });
            let minute = u32::try_from(t).unwrap_or(u32::MAX);
            let cpu = self.cpu[t] + cpu;
            if cpu > self.cpu_limit {
                return Err(GapMiss {
                    resource: GapResource::Cpu,
                    minute,
                    percent: cpu / self.cpu_total * 100.0,
                });
            }
            let memory = self.memory[t] + memory;
            if memory > self.memory_limit {
                return Err(GapMiss {
                    resource: GapResource::Memory,
                    minute,
                    percent: memory / self.memory_total * 100.0,
                });
            }
        }
        Ok(())
    }

    /// Count an admitted candidate from minute zero, as the forecast counts
    /// a reservation.
    pub fn add(&mut self, profile: &ResourceProfile) {
        for (t, minute) in profile.series.iter().take(self.cpu.len()).enumerate() {
            self.cpu[t] += minute.cpu_cores * minute.alive;
            self.memory[t] += minute.memory_bytes as f64 * minute.alive;
        }
    }

    /// `cores` as a share of the host's cores.
    pub fn cpu_percent(&self, cores: f64) -> f64 {
        cores / self.cpu_total * 100.0
    }

    /// `bytes` as a share of the host's memory.
    pub fn memory_percent(&self, bytes: f64) -> f64 {
        bytes / self.memory_total * 100.0
    }
}

/// The forecast load admission may reach: the configured shares of the
/// host's cores and memory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AdmissionLimits {
    pub(crate) cpu_cores: f64,
    pub(crate) memory_bytes: f64,
    host_cores: f64,
    host_memory_bytes: f64,
}

impl AdmissionLimits {
    /// `admit_cpu_percent` of `host`'s cores and `admit_memory_percent` of
    /// its memory.
    pub(crate) fn of(host: &HostLoad, admit_cpu_percent: u8, admit_memory_percent: u8) -> Self {
        let host_cores = f64::from(host.cpu_count);
        let host_memory_bytes = host.memory_total_bytes as f64;
        Self {
            cpu_cores: host_cores * f64::from(admit_cpu_percent) / 100.0,
            memory_bytes: host_memory_bytes * f64::from(admit_memory_percent) / 100.0,
            host_cores,
            host_memory_bytes,
        }
    }
}

/// The resource a candidate would push over its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverResource {
    Cpu,
    Memory,
}

/// The first minute a candidate would push the forecast over a limit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Overload {
    pub(crate) resource: OverResource,
    /// Minutes from now.
    pub(crate) minute: u32,
    /// The forecast with the candidate, as a share of the host's capacity.
    pub(crate) percent: f64,
}

impl fmt::Display for Overload {
    /// `cpu 94% at +6m`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let resource = match self.resource {
            OverResource::Cpu => "cpu",
            OverResource::Memory => "memory",
        };
        write!(
            f,
            "{resource} {:.0}% at +{}m",
            self.percent.round(),
            self.minute
        )
    }
}

/// Whether `candidate` fits under `forecast` within `limits` for every minute
/// of its horizon, or the first minute it would not, CPU before memory.
pub(crate) fn gap_fit(
    forecast: &HostLoadForecast,
    candidate: &ResourceProfile,
    limits: &AdmissionLimits,
) -> Result<(), Overload> {
    let horizon = candidate.horizon_minutes.min(forecast.horizon_minutes);
    for t in 0..=horizon {
        let index = t as usize;
        let (cpu, memory) = candidate.series.get(index).map_or((0.0, 0.0), |minute| {
            (
                minute.cpu_cores * minute.alive,
                minute.memory_bytes as f64 * minute.alive,
            )
        });
        let cpu = forecast.cpu.minutes.get(index).copied().unwrap_or(0.0) + cpu;
        if cpu > limits.cpu_cores {
            return Err(Overload {
                resource: OverResource::Cpu,
                minute: t,
                percent: share(cpu, limits.host_cores),
            });
        }
        let memory = forecast.memory.minutes.get(index).copied().unwrap_or(0.0) + memory;
        if memory > limits.memory_bytes {
            return Err(Overload {
                resource: OverResource::Memory,
                minute: t,
                percent: share(memory, limits.host_memory_bytes),
            });
        }
    }
    Ok(())
}

fn share(value: f64, capacity: f64) -> f64 {
    if capacity > 0.0 {
        value / capacity * 100.0
    } else {
        f64::INFINITY
    }
}
