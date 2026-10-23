//! A forecast's values at fixed offsets, recorded with each drain admission
//! pass so the accuracy report can compare them with the host's measured
//! minutes.
//!
//! The recorded value counts what the pass actually admitted, each as a
//! reservation counts: the host load the forecast expects given what
//! admission did, which is what the measured minute can confirm or refute.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{AdmissionForecastPoint, ForecastDelivery};

use super::{HostLoadForecast, MemoryTerms};
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
    pub baseline_cpu_cores: f64,
    pub baseline_memory_bytes: u64,
    pub covered_deliveries: Vec<ForecastDelivery>,
    /// `(offset, cores, memory)`.
    points: Vec<(u32, f64, MemoryTerms)>,
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
                    *self.memory_terms.get(minute)?,
                ))
            })
            .collect();
        ForecastPoints {
            at: self.at,
            cpu_count: self.host.map(|host| host.cpu_count),
            memory_total_bytes: self.host.map(|host| host.memory_total_bytes),
            baseline_cpu_cores: self.cpu.baseline.unwrap_or(0.0),
            baseline_memory_bytes: self.memory.baseline.unwrap_or(0.0).max(0.0).round() as u64,
            covered_deliveries: self
                .running
                .iter()
                .map(|delivery| ForecastDelivery {
                    workspace_id: delivery.workspace_id.clone(),
                    task_id: delivery.task_id.clone(),
                    admitted_at: delivery.admitted_at,
                    run_ids: delivery.run_ids.clone(),
                })
                .chain(
                    self.reservations
                        .iter()
                        .map(|reservation| ForecastDelivery {
                            workspace_id: reservation.workspace_id.clone(),
                            task_id: reservation.task_id.clone(),
                            admitted_at: reservation.reserved_at,
                            run_ids: Vec::new(),
                        }),
                )
                .collect(),
            points,
            profiles: Arc::clone(&self.profiles),
        }
    }
}

impl ForecastPoints {
    /// Count one admitted delivery, priced by the profile `lookup` finds. A
    /// delivery with no profile adds nothing, as in the forecast.
    pub fn admit(&mut self, task_id: &str, lookup: &ProfileLookup) {
        self.covered_deliveries.push(ForecastDelivery {
            workspace_id: lookup.workspace_id.clone(),
            task_id: task_id.to_string(),
            admitted_at: self.at,
            run_ids: Vec::new(),
        });
        let Some(profile) = self.profiles.lookup(lookup).profile else {
            return;
        };
        for (offset, cores, memory) in &mut self.points {
            let Some(minute) = usize::try_from(*offset)
                .ok()
                .and_then(|minute| profile.series.get(minute))
            else {
                continue;
            };
            *cores += minute.cpu_cores * minute.alive;
            memory.add_profiled(minute, minute.alive);
        }
    }

    /// The points as the store records them.
    pub fn into_points(self) -> Vec<AdmissionForecastPoint> {
        self.points
            .into_iter()
            .map(
                |(offset_minutes, cpu_cores, memory)| AdmissionForecastPoint {
                    offset_minutes,
                    cpu_cores,
                    memory_bytes: memory.bytes().max(0.0).round() as u64,
                },
            )
            .collect()
    }
}
