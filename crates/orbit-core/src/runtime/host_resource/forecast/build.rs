//! Building the forecast from what the host admission lock read.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{AdmissionLoad, AdmissionRun};
use orbit_types::workflow::AUTO_TASK_TAG_PREFIX;

use super::baseline::{CurrentUse, YoungDelivery, baseline};
use super::{ForecastSeries, HostLoad, HostLoadForecast, ReservedAdmission, RunningDelivery};
use crate::runtime::host_resource::delivery::resource_class_in;
use crate::runtime::host_resource::profile::{
    BaseClass, ProfileLookup, ProfileMatch, ProfilePeak, ProfileSet, ResourceProfile,
};
use crate::runtime::run_input::singular_task_id_from_input;

/// The class of a run whose delivery recorded none: assumed to change code,
/// so the lookup falls back to the heavier profiles.
const UNKNOWN_WORK_CLASS: &str = "change";

type PendingSeries = (Vec<f64>, Vec<f64>);

/// The forecast over `0 ..= horizon` minutes from `now`.
pub(crate) fn build_forecast(
    load: &AdmissionLoad,
    profiles: Arc<ProfileSet>,
    host: Option<HostLoad>,
    horizon: u32,
    now: DateTime<Utc>,
) -> HostLoadForecast {
    let points = usize::try_from(horizon)
        .unwrap_or(usize::MAX)
        .saturating_add(1);
    let mut cpu = vec![0.0; points];
    let mut memory = vec![0.0; points];

    let mut running = Vec::new();
    let mut young = Vec::new();
    for delivery in running_deliveries(load, now) {
        let found = profiles.lookup(&running_lookup(&profiles, &delivery));
        let age = delivery.age_minutes as usize;
        let profile = found.profile.as_deref().filter(|profile| {
            profile
                .series
                .get(age)
                .is_some_and(|minute| minute.alive > 0.0)
        });
        match profile {
            Some(profile) => {
                add_running(profile, age, &mut cpu, &mut memory);
                if let Some(started_at) = unrecorded_start(load, &delivery) {
                    young.push(YoungDelivery {
                        started_at,
                        cpu_cores: profile.series[age].cpu_cores,
                        memory_bytes: profile.series[age].memory_bytes as f64,
                    });
                }
            }
            None => {
                for (cores, bytes) in cpu.iter_mut().zip(memory.iter_mut()) {
                    *cores += delivery.current_cpu_cores;
                    *bytes += delivery.current_memory_bytes as f64;
                }
            }
        }
        running.push(RunningDelivery {
            held_flat: profile.is_none(),
            ..with_profile(delivery, &found)
        });
    }

    let mut reservations = Vec::new();
    let mut pending: BTreeMap<(&str, &str), PendingSeries> = BTreeMap::new();
    for reservation in load
        .reservations
        .iter()
        .filter(|reservation| !has_run(load, reservation))
    {
        let lookup = ProfileLookup {
            workspace_id: reservation.workspace_id.clone(),
            work_class: reservation.work_class.clone(),
            complexity: reservation.complexity.clone(),
            base_class: if reservation.base_class == BaseClass::NoDiff.as_str() {
                BaseClass::NoDiff
            } else {
                BaseClass::Change
            },
        };
        let found = profiles.lookup(&lookup);
        if let Some(profile) = found.profile.as_deref() {
            for (t, minute) in profile.series.iter().take(points).enumerate() {
                let cores = minute.cpu_cores * minute.alive;
                let bytes = minute.memory_bytes as f64 * minute.alive;
                if reservation.task_id.starts_with("pending:") {
                    let (cpu, memory) = pending
                        .entry((&reservation.workspace_id, &reservation.task_id))
                        .or_insert_with(|| (vec![0.0; points], vec![0.0; points]));
                    cpu[t] = cpu[t].max(cores);
                    memory[t] = memory[t].max(bytes);
                } else {
                    cpu[t] += cores;
                    memory[t] += bytes;
                }
            }
        }
        reservations.push(ReservedAdmission {
            workspace_id: reservation.workspace_id.clone(),
            task_id: reservation.task_id.clone(),
            work_class: reservation.work_class.clone(),
            complexity: reservation.complexity.clone(),
            reserved_at: reservation.reserved_at,
            profile_source: found.source,
            profile_key: found.profile.as_ref().map(|profile| profile.key.clone()),
        });
    }

    for (pending_cpu, pending_memory) in pending.into_values() {
        for t in 0..points {
            cpu[t] += pending_cpu[t];
            memory[t] += pending_memory[t];
        }
    }

    let current = CurrentUse {
        cpu_cores: running.iter().map(|d| d.current_cpu_cores).sum(),
        memory_bytes: running
            .iter()
            .fold(0u64, |sum, d| sum.saturating_add(d.current_memory_bytes)),
    };
    let baseline = baseline(load, host, &current, &young, now);

    HostLoadForecast {
        at: now,
        horizon_minutes: horizon,
        host,
        baseline_minute: baseline.minute,
        running,
        reservations,
        cpu: series(baseline.cpu, cpu),
        memory: series(baseline.memory, memory),
        profiles,
    }
}

/// When a delivery whose live runs have not recorded a minute started: the
/// earliest start of its live runs. `None` once one has recorded a minute, or
/// while none has started.
fn unrecorded_start(load: &AdmissionLoad, delivery: &RunningDelivery) -> Option<DateTime<Utc>> {
    let runs = load.runs.iter().filter(|run| {
        run.live
            && run.workspace_id == delivery.workspace_id
            && delivery.run_ids.contains(&run.run_id)
    });
    runs.clone()
        .all(|run| run.newest_minute.is_none())
        .then(|| runs.filter_map(|run| run.started_at).min())
        .flatten()
}

/// `P_i(a_i + t) · alive_i(a_i + t) / alive_i(a_i)` for each `t`.
fn add_running(profile: &ResourceProfile, age: usize, cpu: &mut [f64], memory: &mut [f64]) {
    let alive_now = profile.series[age].alive;
    for (t, minute) in profile.series[age..].iter().take(cpu.len()).enumerate() {
        let still_running = minute.alive / alive_now;
        cpu[t] += minute.cpu_cores * still_running;
        memory[t] += minute.memory_bytes as f64 * still_running;
    }
}

fn series(baseline: Option<f64>, mut minutes: Vec<f64>) -> ForecastSeries {
    let flat = baseline.unwrap_or(0.0);
    for value in &mut minutes {
        *value += flat;
    }
    let peak = minutes.iter().enumerate().fold(
        ProfilePeak {
            minute: 0,
            value: minutes.first().copied().unwrap_or(0.0),
        },
        |peak, (minute, value)| {
            if *value > peak.value {
                ProfilePeak {
                    minute: u32::try_from(minute).unwrap_or(u32::MAX),
                    value: *value,
                }
            } else {
                peak
            }
        },
    );
    ForecastSeries {
        baseline,
        minutes,
        peak,
    }
}

/// The task a delivery run names: its input's, else its newest minute's.
fn run_task(run: &AdmissionRun) -> Option<&str> {
    run.input
        .as_ref()
        .and_then(singular_task_id_from_input)
        .or_else(|| run.newest_minute.as_ref().map(|m| m.task_id.as_str()))
}

/// Whether a delivery run naming the reservation's task was created in its
/// workspace since it was written.
fn has_run(
    load: &AdmissionLoad,
    reservation: &orbit_store::contracts::AdmissionReservation,
) -> bool {
    load.runs.iter().any(|run| {
        run.workspace_id == reservation.workspace_id
            && run.created_at >= reservation.reserved_at
            && run_task(run) == Some(reservation.task_id.as_str())
    })
}

/// Live delivery runs grouped per workspace and task.
fn running_deliveries(load: &AdmissionLoad, now: DateTime<Utc>) -> Vec<RunningDelivery> {
    let mut deliveries: BTreeMap<(String, String), RunningDelivery> = BTreeMap::new();
    // Oldest run first, so a chain's first recorded class names the delivery.
    for run in load.runs.iter().filter(|run| run.live) {
        let Some(task_id) = run_task(run) else {
            continue;
        };
        let class = run
            .input
            .as_ref()
            .and_then(|input| resource_class_in(input, task_id))
            .map(|class| (class.work_class, class.complexity, class.admitted_at))
            .or_else(|| {
                run.newest_minute.as_ref().map(|minute| {
                    (
                        minute.work_class.clone(),
                        minute.complexity.clone(),
                        minute.delivery_admitted_at.unwrap_or(run.created_at),
                    )
                })
            });
        let delivery = deliveries
            .entry((run.workspace_id.clone(), task_id.to_string()))
            .or_insert_with(|| RunningDelivery {
                workspace_id: run.workspace_id.clone(),
                task_id: task_id.to_string(),
                run_ids: Vec::new(),
                work_class: String::new(),
                complexity: None,
                admitted_at: run.created_at,
                age_minutes: 0,
                profile_source: crate::runtime::host_resource::profile::ProfileSource::None,
                profile_key: None,
                current_cpu_cores: 0.0,
                current_memory_bytes: 0,
                held_flat: false,
            });
        delivery.run_ids.push(run.run_id.clone());
        let admitted_at = match class {
            Some((work_class, complexity, admitted_at)) => {
                if delivery.work_class.is_empty() {
                    delivery.work_class = work_class;
                    delivery.complexity = complexity;
                }
                admitted_at
            }
            None => run.created_at,
        };
        delivery.admitted_at = delivery.admitted_at.min(admitted_at);
        if let Some(minute) = &run.newest_minute {
            delivery.current_cpu_cores += minute.cpu_cores;
            delivery.current_memory_bytes = delivery
                .current_memory_bytes
                .saturating_add(minute.memory_bytes);
        }
    }
    deliveries
        .into_values()
        .map(|mut delivery| {
            if delivery.work_class.is_empty() {
                delivery.work_class = UNKNOWN_WORK_CLASS.to_string();
            }
            let seconds = (now - delivery.admitted_at).num_seconds().max(0);
            delivery.age_minutes = u32::try_from(seconds / 60).unwrap_or(u32::MAX);
            delivery
        })
        .collect()
}

/// A running delivery's lookup. Its run records no base class, so a template
/// takes the one its own profile learned, and is assumed to change code
/// without one.
fn running_lookup(profiles: &ProfileSet, delivery: &RunningDelivery) -> ProfileLookup {
    let no_diff = delivery.work_class.starts_with(AUTO_TASK_TAG_PREFIX)
        && profiles.profiles.iter().any(|profile| {
            profile.key.workspace_id == delivery.workspace_id
                && profile.key.work_class == delivery.work_class
                && profile.base_class == BaseClass::NoDiff
        });
    ProfileLookup::new(
        delivery.workspace_id.clone(),
        delivery.work_class.clone(),
        delivery.complexity.clone(),
        no_diff,
    )
}

fn with_profile(delivery: RunningDelivery, found: &ProfileMatch) -> RunningDelivery {
    RunningDelivery {
        profile_source: found.source,
        profile_key: found.profile.as_ref().map(|profile| profile.key.clone()),
        ..delivery
    }
}
