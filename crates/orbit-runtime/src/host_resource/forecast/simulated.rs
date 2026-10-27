//! A host load forecast over simulated deliveries, for the backlog forecast's
//! resource fit [ORB-18689].
//!
//! The backlog forecast replays gap fit inside each simulated drain pass, so
//! it needs the forecast gap fit decides against without the host admission
//! lock or a live reading: the host's size and baseline are captured once per
//! backlog forecast, and the running set is the simulated one. Each running
//! delivery is priced as `build_forecast` prices a
//! live one, `P(a + t) · alive(a + t) / alive(a)` from its profile at its
//! simulated age. One older than its profile's last minute is held flat at
//! that minute's use, as a live one is held at its measured use; one without
//! a profile adds nothing, as no measured use exists for it. A simulated
//! forecast holds no reservations.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use super::build::{add_running, series};
use super::{HostCapacity, HostLoad, HostLoadForecast, MemoryTerms, RunningDelivery};
use crate::host_resource::profile::{ProfileMatch, ProfileSet};

/// The class a simulated delivery without a profile is recorded under.
const UNPROFILED_WORK_CLASS: &str = "change";

/// The host a simulated drain runs on, captured once per backlog forecast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulatedHost {
    pub capacity: HostCapacity,
    /// Load outside deliveries, held flat. `None` when no recorded forecast
    /// measured it; it then counts as zero, as an unknown baseline does in
    /// the live forecast.
    pub baseline_cpu_cores: Option<f64>,
    pub baseline_memory_bytes: Option<f64>,
}

/// A delivery a simulated drain is running.
#[derive(Debug, Clone, Copy)]
pub struct SimulatedDelivery<'a> {
    pub workspace_id: &'a str,
    pub task_id: &'a str,
    /// The profile its admission was priced by.
    pub profile: &'a ProfileMatch,
    /// Whole minutes since its simulated admission.
    pub age_minutes: u32,
}

/// The forecast over `0 ..= horizon` minutes from `at` with `deliveries`
/// running on `host`.
pub fn simulated_forecast(
    at: DateTime<Utc>,
    host: &SimulatedHost,
    deliveries: &[SimulatedDelivery<'_>],
    profiles: Arc<ProfileSet>,
    horizon: u32,
) -> HostLoadForecast {
    let points = usize::try_from(horizon)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut cpu = vec![0.0; points];
    let mut memory = vec![MemoryTerms::default(); points];
    let mut running = Vec::with_capacity(deliveries.len());
    for delivery in deliveries {
        let age = delivery.age_minutes as usize;
        let profile = delivery.profile.profile.as_deref();
        let live = profile.filter(|profile| {
            profile
                .series
                .get(age)
                .is_some_and(|minute| minute.alive > 0.0)
        });
        let (current_cpu_cores, current_memory_bytes) = match live {
            Some(profile) => {
                add_running(profile, age, &mut cpu, &mut memory);
                let minute = &profile.series[age];
                (minute.cpu_cores, minute.memory_bytes)
            }
            None => {
                let last = profile
                    .and_then(|profile| profile.series.iter().rev().find(|m| m.alive > 0.0))
                    .map_or((0.0, 0), |minute| (minute.cpu_cores, minute.memory_bytes));
                for (cores, terms) in cpu.iter_mut().zip(memory.iter_mut()) {
                    *cores += last.0;
                    terms.add_flat(last.1 as f64);
                }
                last
            }
        };
        let key = profile.map(|profile| &profile.key);
        running.push(RunningDelivery {
            workspace_id: delivery.workspace_id.to_string(),
            task_id: delivery.task_id.to_string(),
            run_ids: Vec::new(),
            work_class: key.map_or_else(
                || UNPROFILED_WORK_CLASS.to_string(),
                |key| key.work_class.clone(),
            ),
            complexity: key.and_then(|key| key.complexity.clone()),
            admitted_at: at - Duration::minutes(i64::from(delivery.age_minutes)),
            age_minutes: delivery.age_minutes,
            profile_source: delivery.profile.source,
            profile_key: key.cloned(),
            current_cpu_cores,
            current_memory_bytes,
            held_flat: live.is_none(),
        });
    }
    for cores in &mut cpu {
        *cores += host.baseline_cpu_cores.unwrap_or(0.0);
    }
    for terms in &mut memory {
        terms.add_flat(host.baseline_memory_bytes.unwrap_or(0.0));
    }
    HostLoadForecast {
        at,
        horizon_minutes: horizon,
        host: Some(HostLoad {
            cpu_cores: host.baseline_cpu_cores,
            sustained_cpu: None,
            cpu_count: host.capacity.cpu_count,
            memory_bytes: host.baseline_memory_bytes.unwrap_or(0.0).round() as u64,
            memory_total_bytes: host.capacity.memory_total_bytes,
        }),
        baseline_minute: None,
        running,
        reservations: Vec::new(),
        cpu: series(host.baseline_cpu_cores, cpu),
        memory: series(
            host.baseline_memory_bytes,
            memory.iter().map(MemoryTerms::bytes).collect(),
        ),
        memory_terms: memory,
        profiles,
    }
}
