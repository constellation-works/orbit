//! The baseline `B`: the host's load that is not from deliveries.
//!
//! The host and the runs are measured over the same window, so neither a
//! short host spike nor a run's minute boundary leaves load that belongs to
//! neither:
//!
//! - **Settled minute.** The newest recorded host minute that ended at most
//!   [`SETTLED_MINUTE_MAX_AGE`] ago and is covered by every run still
//!   recording: each live run's newest minute reaches its end, and no run
//!   that has not recorded its first minute yet had started by then. From the
//!   host's CPU mean and memory peak in it, each run minute overlapping it is
//!   subtracted in proportion to the seconds of it that minute covered. A
//!   minute covers from its start for its sample count times
//!   [`RESOURCE_SAMPLE_INTERVAL`], at most a minute, and never past its run's
//!   `finished_at`, so a run that ended early in its last minute is not
//!   subtracted as if it ran to the end of it.
//! - **Live floor.** Load that began after the settled minute, such as a
//!   manual build, is the live reading's part. CPU uses the probe's sustained
//!   reading over the trailing one to two minutes, minus the run minutes over
//!   the same window, with each live run's newest minute extended to now and a
//!   delivery that has not recorded a minute counted from its start; a
//!   short spike moves it by its length over the window's. Memory uses the
//!   live reading minus the running deliveries' current use. `B` is the
//!   larger of the settled and live values, so the live part can only add
//!   load.
//! - **No settled minute** (no worker recorded one lately, or a run blocks
//!   every recent one): the live reading over the probe's short window minus
//!   the running deliveries' current use, as before.
//!
//! A delivery whose live runs have not recorded a minute has its profile
//! added in full while the live reading already holds its startup load. The
//! live values subtract its profile's expected use at its age instead: over
//! the part of the sustained window it was running, and otherwise in
//! proportion to the share of its first minute that has elapsed, so one
//! created a moment ago subtracts almost nothing. A settled minute ends
//! before such a delivery started while it can still record one, so where
//! one exists the subtraction can at most bring `B` down to the settled
//! value: it is capped at what the host grew by since.

use chrono::{DateTime, TimeDelta, Utc};
use orbit_store::contracts::{AdmissionLoad, AdmissionRunMinute, HostResourceMinute};

use super::HostLoad;
use crate::runtime::host_resource::RESOURCE_SAMPLE_INTERVAL;

const MINUTE: TimeDelta = TimeDelta::minutes(1);
/// A recorded host minute that ended longer ago than this no longer says what
/// the host does now.
const SETTLED_MINUTE_MAX_AGE: TimeDelta = TimeDelta::minutes(3);
/// A run whose newest minute ended longer ago than this, or that started this
/// long ago without recording one, is not recording: its next minute is not
/// waited for.
const RECORDING_GRACE: TimeDelta = TimeDelta::seconds(90);

/// A running delivery whose live runs have not recorded a minute.
pub(super) struct YoungDelivery {
    /// When its earliest started live run started.
    pub(super) started_at: DateTime<Utc>,
    /// Its profile's expected use at its age.
    pub(super) cpu_cores: f64,
    pub(super) memory_bytes: f64,
}

/// What the running deliveries use now: their live runs' newest minutes.
pub(super) struct CurrentUse {
    pub(super) cpu_cores: f64,
    pub(super) memory_bytes: u64,
}

/// `B` for CPU and memory, and the host minute it was measured over.
pub(super) struct Baseline {
    pub(super) cpu: Option<f64>,
    pub(super) memory: Option<f64>,
    pub(super) minute: Option<DateTime<Utc>>,
}

pub(super) fn baseline(
    load: &AdmissionLoad,
    host: Option<HostLoad>,
    current: &CurrentUse,
    young: &[YoungDelivery],
    now: DateTime<Utc>,
) -> Baseline {
    let Some(host) = host else {
        return Baseline {
            cpu: None,
            memory: None,
            minute: None,
        };
    };
    // A delivery with no minute is assumed to have reached the elapsed share
    // of its first minute's profile.
    let (young_cpu, young_memory) = young.iter().fold((0.0, 0.0), |(cpu, memory), delivery| {
        let elapsed = share(now - delivery.started_at, MINUTE);
        (
            cpu + delivery.cpu_cores * elapsed,
            memory + delivery.memory_bytes * elapsed,
        )
    });
    // Memory stays in whole bytes, as measured.
    let live_memory = (host.memory_bytes as f64 - current.memory_bytes as f64 - young_memory)
        .max(0.0)
        .round();
    let Some(minute) = settled_minute(load, now) else {
        return Baseline {
            cpu: host
                .cpu_cores
                .map(|cores| (cores - current.cpu_cores - young_cpu).max(0.0)),
            memory: Some(live_memory),
            minute: None,
        };
    };
    let start = minute.minute_at;
    let end = start + MINUTE;
    let (mut run_cpu, mut run_memory) = (0.0, 0.0);
    for run_minute in &load.run_minutes {
        let covered = share(overlap(covered_span(run_minute), (start, end)), MINUTE);
        run_cpu += run_minute.cpu_cores * covered;
        run_memory += run_minute.memory_bytes as f64 * covered;
    }
    let settled_cpu = (minute.cpu_cores - run_cpu).max(0.0);
    let settled_memory = (minute.memory_bytes as f64 - run_memory).max(0.0).round();
    let live_cpu = host.sustained_cpu.map(|sustained| {
        (sustained.cores - window_task_cpu(load, young, (sustained.since, now))).max(0.0)
    });
    Baseline {
        cpu: Some(live_cpu.map_or(settled_cpu, |live| live.max(settled_cpu))),
        memory: Some(live_memory.max(settled_memory)),
        minute: Some(start),
    }
}

/// The newest recorded host minute every recording run covers.
fn settled_minute(load: &AdmissionLoad, now: DateTime<Utc>) -> Option<&HostResourceMinute> {
    let recording = now - RECORDING_GRACE;
    let covered_until = load
        .runs
        .iter()
        .filter(|run| run.live)
        .filter_map(|run| match &run.newest_minute {
            Some(newest) => Some(newest.minute_started_at + MINUTE),
            // A run uses nothing before it starts.
            None => run.started_at,
        })
        .filter(|until| *until >= recording)
        .fold(now, DateTime::min);
    load.host_minutes.iter().rev().find(|minute| {
        let end = minute.minute_at + MINUTE;
        end <= covered_until && now - end <= SETTLED_MINUTE_MAX_AGE
    })
}

/// Mean cores the deliveries used over `window`: recorded minutes, each live
/// run's newest minute extended from its end to now, and each delivery with
/// no minute at its expected use from its start.
fn window_task_cpu(
    load: &AdmissionLoad,
    young: &[YoungDelivery],
    window: (DateTime<Utc>, DateTime<Utc>),
) -> f64 {
    let length = window.1 - window.0;
    let recorded: f64 = load
        .run_minutes
        .iter()
        .map(|minute| minute.cpu_cores * share(overlap(covered_span(minute), window), length))
        .sum();
    let extended: f64 = load
        .runs
        .iter()
        .filter(|run| run.live)
        .filter_map(|run| run.newest_minute.as_ref())
        .map(|newest| {
            let from = newest.minute_started_at + sampled(newest.samples);
            newest.cpu_cores * share(overlap((from, window.1), window), length)
        })
        .sum();
    let unrecorded: f64 = young
        .iter()
        .map(|delivery| {
            delivery.cpu_cores * share(overlap((delivery.started_at, window.1), window), length)
        })
        .sum();
    recorded + extended + unrecorded
}

/// The wall time a run minute's figures cover: from its start for its samples'
/// interval, at most a minute, and not past its run's end.
fn covered_span(minute: &AdmissionRunMinute) -> (DateTime<Utc>, DateTime<Utc>) {
    let mut end = minute.minute_started_at + sampled(minute.samples);
    if let Some(finished) = minute.run_finished_at {
        end = end.min(finished);
    }
    (minute.minute_started_at, end)
}

/// The most of a minute `samples` readings cover.
fn sampled(samples: u32) -> TimeDelta {
    TimeDelta::from_std(RESOURCE_SAMPLE_INTERVAL)
        .unwrap_or(MINUTE)
        .checked_mul(i32::try_from(samples).unwrap_or(i32::MAX))
        .unwrap_or(MINUTE)
        .min(MINUTE)
}

fn overlap(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> TimeDelta {
    (a.1.min(b.1) - a.0.max(b.0)).max(TimeDelta::zero())
}

/// `part / whole`, within `0 ..= 1`.
fn share(part: TimeDelta, whole: TimeDelta) -> f64 {
    let whole = whole.num_milliseconds();
    if whole <= 0 {
        return 0.0;
    }
    (part.num_milliseconds() as f64 / whole as f64).clamp(0.0, 1.0)
}
