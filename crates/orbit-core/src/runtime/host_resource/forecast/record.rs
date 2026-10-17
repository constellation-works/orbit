//! A forecast's values at fixed offsets, recorded with each drain admission
//! pass so the accuracy report can compare them with the host's measured
//! minutes.
//!
//! The recorded value counts what the pass actually admitted, each at
//! `P(t) · alive(t)` as a reservation counts, memory summed as the forecast
//! sums it: the host load the forecast expects given what admission did,
//! which is what the measured minute can confirm or refute.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_store::contracts::AdmissionForecastPoint;

use super::HostLoadForecast;
use super::memory::MemorySum;
use crate::runtime::host_resource::profile::{ProfileLookup, ProfileSet};

/// Minutes after a pass at which its forecast is recorded. An offset past the
/// forecast's horizon is not recorded.
pub const ACCURACY_OFFSETS: [u32; 4] = [5, 10, 20, 30];

/// One forecast at [`ACCURACY_OFFSETS`], to which admitted tasks are added.
#[derive(Debug, Clone)]
pub struct ForecastPoints {
    pub at: DateTime<Utc>,
    /// The host's size; `None` when it could not be read.
    pub cpu_count: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    /// `(offset, cores, memory)`.
    points: Vec<(u32, f64, MemorySum)>,
    profiles: Arc<ProfileSet>,
}

impl HostLoadForecast {
    /// This forecast at [`ACCURACY_OFFSETS`], before anything is admitted
    /// against it.
    pub fn accuracy_points(&self) -> ForecastPoints {
        let points = ACCURACY_OFFSETS
            .iter()
            .filter_map(|&offset| {
                let minute = usize::try_from(offset).ok()?;
                Some((
                    offset,
                    *self.cpu.minutes.get(minute)?,
                    *self.memory_sums.get(minute)?,
                ))
            })
            .collect();
        ForecastPoints {
            at: self.at,
            cpu_count: self.host.map(|host| host.cpu_count),
            memory_total_bytes: self.host.map(|host| host.memory_total_bytes),
            points,
            profiles: Arc::clone(&self.profiles),
        }
    }
}

impl ForecastPoints {
    /// Count one admitted delivery, priced by the profile `lookup` finds. A
    /// delivery with no profile adds nothing, as in the forecast.
    pub fn admit(&mut self, lookup: &ProfileLookup) {
        let Some(profile) = self.profiles.lookup(lookup).profile else {
            return;
        };
        for (offset, cores, bytes) in &mut self.points {
            let Some(minute) = usize::try_from(*offset)
                .ok()
                .and_then(|minute| profile.series.get(minute))
            else {
                continue;
            };
            *cores += minute.cpu_cores * minute.alive;
            bytes.add_minute(minute, minute.alive);
        }
    }

    /// The points as the store records them.
    pub fn into_points(self) -> Vec<AdmissionForecastPoint> {
        self.points
            .into_iter()
            .map(
                |(offset_minutes, cpu_cores, bytes)| AdmissionForecastPoint {
                    offset_minutes,
                    cpu_cores,
                    memory_bytes: bytes.bytes().max(0.0).round() as u64,
                },
            )
            .collect()
    }
}
