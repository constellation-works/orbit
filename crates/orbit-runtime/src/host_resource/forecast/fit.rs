//! Gap fit: whether a candidate's profile fits under the forecast.

use super::{HostLoadForecast, MemoryTerms};
use crate::host_resource::profile::ResourceProfile;

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
    memory: Vec<MemoryTerms>,
    cpu_total: f64,
    memory_total: f64,
    cpu_limit: f64,
    memory_limit: f64,
    /// No running delivery or reservation is in the forecast, and nothing
    /// has been added to it.
    idle: bool,
}

/// How a candidate that does not fit was admitted anyway: the forecast held
/// no running delivery or reservation, so no work on the host would make
/// room by finishing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IdleHostAdmission {
    /// Where it first exceeds its threshold: on its own when `alone`, else
    /// over the forecast, which on an idle host is the baseline.
    pub miss: GapMiss,
    /// Whether its profile exceeds a threshold on its own; otherwise only the
    /// load outside deliveries pushed it over.
    pub alone: bool,
}

impl IdleHostAdmission {
    /// `cpu 125% at +3m on its own; admitted on an idle host`.
    pub fn describe(&self) -> String {
        format!(
            "{} {}; admitted on an idle host",
            self.miss.describe(),
            if self.alone {
                "on its own"
            } else {
                "with the host baseline"
            }
        )
    }
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
            memory: self.memory_terms.clone(),
            cpu_total,
            memory_total,
            cpu_limit: cpu_total * f64::from(admit_cpu_percent) / 100.0,
            memory_limit: memory_total * f64::from(admit_memory_percent) / 100.0,
            idle: self.running.is_empty() && self.reservations.is_empty(),
        })
    }
}

impl ForecastGap {
    /// Whether, for every `t` in `0 ..= H` of `profile`'s horizon,
    /// `F(t) + P.cpu(t) · alive(t)` and the memory forecast with the
    /// candidate counted in its summed quantile stay at or under their
    /// thresholds; otherwise the first minute that does not, CPU before
    /// memory.
    ///
    /// A candidate that does not fit is still admitted while the gap is
    /// idle: no running delivery or reservation would make room for it by
    /// finishing, so refusing it would only hold it, and whatever waits
    /// behind it, for good. The resource throttle remains the backstop.
    /// That admission is `Ok(Some(_))`, with the miss it was admitted over.
    pub fn fit(&self, profile: &ResourceProfile) -> Result<Option<IdleHostAdmission>, GapMiss> {
        let Some(miss) = self.first_miss(profile, true) else {
            return Ok(None);
        };
        if !self.idle {
            return Err(miss);
        }
        Ok(Some(match self.first_miss(profile, false) {
            Some(alone) => IdleHostAdmission {
                miss: alone,
                alone: true,
            },
            None => IdleHostAdmission { miss, alone: false },
        }))
    }

    /// The first minute `profile`, over the forecast or over an empty one,
    /// exceeds a threshold.
    fn first_miss(&self, profile: &ResourceProfile, over_forecast: bool) -> Option<GapMiss> {
        let last = usize::try_from(profile.horizon_minutes)
            .unwrap_or(usize::MAX)
            .min(self.cpu.len().saturating_sub(1));
        for t in 0..=last {
            let own = profile.series.get(t);
            let minute = u32::try_from(t).unwrap_or(u32::MAX);
            let cpu = if over_forecast { self.cpu[t] } else { 0.0 }
                + own.map_or(0.0, |own| own.cpu_cores * own.alive);
            if cpu > self.cpu_limit {
                return Some(GapMiss {
                    resource: GapResource::Cpu,
                    minute,
                    percent: cpu / self.cpu_total * 100.0,
                });
            }
            let forecast = if over_forecast {
                self.memory[t]
            } else {
                MemoryTerms::default()
            };
            let memory = own
                .map_or(forecast, |own| forecast.with_profiled(own, own.alive))
                .bytes();
            if memory > self.memory_limit {
                return Some(GapMiss {
                    resource: GapResource::Memory,
                    minute,
                    percent: memory / self.memory_total * 100.0,
                });
            }
        }
        None
    }

    /// Count an admitted candidate from minute zero, as the forecast counts
    /// a reservation. The gap is no longer idle, even for a candidate
    /// without a profile.
    pub fn add(&mut self, profile: Option<&ResourceProfile>) {
        self.idle = false;
        for (t, minute) in profile
            .into_iter()
            .flat_map(|profile| &profile.series)
            .take(self.cpu.len())
            .enumerate()
        {
            self.cpu[t] += minute.cpu_cores * minute.alive;
            self.memory[t].add_profiled(minute, minute.alive);
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
