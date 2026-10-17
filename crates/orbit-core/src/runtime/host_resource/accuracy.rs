//! How well the recorded admission forecasts matched the host's measured
//! load, and how far shadow decisions differed from what admission did.
//!
//! - **Pairs.** A point forecast at `forecast_at` for `+n` minutes pairs with
//!   the `host_resource_minutes` row of the minute starting at
//!   `forecast_at`, truncated to the minute, plus `n`. A point whose minute
//!   has not fully ended at report time is pending, even with a partial
//!   measurement. Ended minutes without a row are missing: `unsampled` when
//!   no delivery worker was live in that minute, and `gap` when one was.
//!   Pending and missing both count as unmeasured; only ended, measured
//!   minutes contribute error pairs.
//! - **Error** is forecast minus measured, so a negative error is an
//!   under-forecast: the host carried more than admission expected.
//! - **Shadow decisions.** Per drain, over its `shadow` passes: the tasks
//!   admission admitted that gap fit would have held, and the tasks gap fit
//!   would have admitted that admission did not. Each task is counted once
//!   per drain, however many passes it appeared in. Pull drains separately
//!   count request forecasts, requests sent, and shadow requests enforce
//!   would have held because no work fit, even without a returned claim.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, DurationRound, Utc};
use orbit_store::contracts::{AdmissionForecastRecord, DeliveryRunInterval, HostResourceMinute};
use serde::Serialize;

use super::forecast::ACCURACY_OFFSETS;
use super::profile::PROFILE_HISTORY_DAYS;

const SHADOW: &str = "shadow";

/// An operator-selected cohort of stored forecasts, with inclusive time bounds.
/// Drain and mode are exact matches; these rows do not attest source or settings.
#[derive(Debug, Clone, Serialize)]
pub struct AccuracySelection {
    pub since: DateTime<Utc>,
    pub drain: Option<String>,
    pub mode: Option<String>,
}

impl AccuracySelection {
    /// The legacy population: all drains and modes over the last 30 days.
    pub fn last_30_days(report_at: DateTime<Utc>) -> Self {
        Self {
            since: report_at - Duration::days(PROFILE_HISTORY_DAYS),
            drain: None,
            mode: None,
        }
    }
}

/// The forecast accuracy report.
#[derive(Debug, Clone, Serialize)]
pub struct ForecastAccuracy {
    /// The oldest forecast considered (inclusive); retained for compatibility.
    pub since: DateTime<Utc>,
    /// One snapshot for the forecast upper bound and target-minute maturity.
    pub report_at: DateTime<Utc>,
    /// Effective selection, applied to points, errors and decisions alike.
    pub selection: AccuracySelection,
    /// Recorded admission passes.
    pub forecasts: u64,
    /// Accuracy at each recorded offset.
    pub offsets: Vec<OffsetAccuracy>,
    /// Shadow decisions against actual admissions, per drain.
    pub drains: Vec<DrainShadowDecisions>,
    /// Every finalized forecast/measured-minute pair, oldest first.
    pub pairs: Vec<ForecastPair>,
}

/// One offset's forecast points against the measured host.
#[derive(Debug, Clone, Serialize)]
pub struct OffsetAccuracy {
    pub offset_minutes: u32,
    pub paired: u64,
    /// Compatible total of pending and missing points.
    pub unmeasured: u64,
    /// Target minutes not fully ended at report time, including partial rows.
    pub pending: u64,
    /// Ended target minutes with no measured row. The sum of [`Self::unsampled`]
    /// and [`Self::gap`].
    pub missing: u64,
    /// Ended target minutes with no host row and no delivery run live.
    pub unsampled: u64,
    /// Ended target minutes with a delivery run live but no host row.
    pub gap: u64,
    /// In cores.
    pub cpu: ErrorSummary,
    /// In bytes.
    pub memory: ErrorSummary,
}

/// Forecast errors over one offset's pairs; every field is `None` without
/// pairs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ErrorSummary {
    /// Mean of forecast minus measured.
    pub mean_error: Option<f64>,
    pub mean_absolute_error: Option<f64>,
    /// Share of pairs, `0..=1`, whose forecast was below the measured value.
    pub under_rate: Option<f64>,
    /// The pair with the largest absolute error.
    pub worst_miss: Option<WorstMiss>,
}

/// The largest miss at one offset.
#[derive(Debug, Clone, Serialize)]
pub struct WorstMiss {
    /// Forecast minus measured.
    pub error: f64,
    pub forecast: f64,
    pub measured: f64,
    pub forecast_at: DateTime<Utc>,
    pub minute_at: DateTime<Utc>,
    pub workspace_id: String,
    pub run_id: String,
}

/// One forecast point and the host minute it forecast.
#[derive(Debug, Clone, Serialize)]
pub struct ForecastPair {
    pub workspace_id: String,
    pub run_id: String,
    pub mode: String,
    pub forecast_at: DateTime<Utc>,
    pub offset_minutes: u32,
    pub minute_at: DateTime<Utc>,
    pub forecast_cpu_cores: f64,
    pub measured_cpu_cores: f64,
    pub forecast_memory_bytes: u64,
    pub measured_memory_bytes: u64,
}

/// One drain's shadow decisions against what it admitted.
#[derive(Debug, Clone, Serialize)]
pub struct DrainShadowDecisions {
    pub workspace_id: String,
    pub run_id: String,
    pub shadow_passes: u64,
    pub enforce_passes: u64,
    /// Tasks the drain admitted during its shadow passes.
    pub admitted: u64,
    /// Admitted while gap fit would have held them.
    pub would_hold: Vec<String>,
    /// Gap fit would have admitted them in a pass that did not.
    pub would_add: Vec<String>,
    /// Pull request forecasts, including ones enforce held.
    pub pull_requests: u64,
    /// Requests actually sent to the owner, in either mode.
    pub pull_sent: u64,
    /// Shadow requests enforce would have held (`forecast_no_headroom`).
    pub pull_would_hold: u64,
}

/// The report over preselected `records` against the host's measured `minutes`.
///
/// `deliveries` are the workers that could have sampled the host. An ended
/// minute with no row is a `gap` when one of them was live during it, and
/// `unsampled` otherwise. A drain that only recorded forecasts is not a
/// delivery.
pub(crate) fn forecast_accuracy(
    records: &[AdmissionForecastRecord],
    minutes: &[HostResourceMinute],
    deliveries: &[DeliveryRunInterval],
    selection: AccuracySelection,
    report_at: DateTime<Utc>,
) -> ForecastAccuracy {
    let measured: BTreeMap<DateTime<Utc>, &HostResourceMinute> = minutes
        .iter()
        .map(|minute| (minute.minute_at, minute))
        .collect();
    let mut pairs = Vec::new();
    let mut pending: BTreeMap<u32, u64> = BTreeMap::new();
    let mut unsampled: BTreeMap<u32, u64> = BTreeMap::new();
    let mut gap: BTreeMap<u32, u64> = BTreeMap::new();
    for record in records {
        let start = record
            .forecast_at
            .duration_trunc(Duration::minutes(1))
            .unwrap_or(record.forecast_at);
        for point in &record.points {
            let minute_at = start + Duration::minutes(i64::from(point.offset_minutes));
            if minute_at + Duration::minutes(1) > report_at {
                *pending.entry(point.offset_minutes).or_default() += 1;
                continue;
            }
            let Some(host) = measured.get(&minute_at) else {
                let bucket = if deliveries
                    .iter()
                    .any(|delivery| delivery_covers(delivery, minute_at))
                {
                    &mut gap
                } else {
                    &mut unsampled
                };
                *bucket.entry(point.offset_minutes).or_default() += 1;
                continue;
            };
            pairs.push(ForecastPair {
                workspace_id: record.workspace_id.clone(),
                run_id: record.run_id.clone(),
                mode: record.mode.clone(),
                forecast_at: record.forecast_at,
                offset_minutes: point.offset_minutes,
                minute_at,
                forecast_cpu_cores: point.cpu_cores,
                measured_cpu_cores: host.cpu_cores,
                forecast_memory_bytes: point.memory_bytes,
                measured_memory_bytes: host.memory_bytes,
            });
        }
    }

    let mut offsets: BTreeSet<u32> = ACCURACY_OFFSETS.into_iter().collect();
    offsets.extend(pairs.iter().map(|pair| pair.offset_minutes));
    offsets.extend(pending.keys().copied());
    offsets.extend(unsampled.keys().copied());
    offsets.extend(gap.keys().copied());
    let offsets = offsets
        .into_iter()
        .map(|offset| {
            let at_offset: Vec<&ForecastPair> = pairs
                .iter()
                .filter(|pair| pair.offset_minutes == offset)
                .collect();
            let pending = pending.get(&offset).copied().unwrap_or(0);
            let unsampled = unsampled.get(&offset).copied().unwrap_or(0);
            let gap = gap.get(&offset).copied().unwrap_or(0);
            let missing = unsampled + gap;
            OffsetAccuracy {
                offset_minutes: offset,
                paired: at_offset.len() as u64,
                unmeasured: pending + missing,
                pending,
                missing,
                unsampled,
                gap,
                cpu: summarize(&at_offset, |pair| {
                    (pair.forecast_cpu_cores, pair.measured_cpu_cores)
                }),
                memory: summarize(&at_offset, |pair| {
                    (
                        pair.forecast_memory_bytes as f64,
                        pair.measured_memory_bytes as f64,
                    )
                }),
            }
        })
        .collect();

    ForecastAccuracy {
        since: selection.since,
        report_at,
        selection,
        forecasts: records.len() as u64,
        offsets,
        drains: shadow_decisions(records),
        pairs,
    }
}

/// A delivery covers a minute when it had started before the minute ended
/// and had not finished before the minute started. Finishing exactly at
/// `minute_at`, or starting exactly as the minute ends, does not cover it.
fn delivery_covers(delivery: &DeliveryRunInterval, minute_at: DateTime<Utc>) -> bool {
    let end = minute_at + Duration::minutes(1);
    delivery.started_at < end
        && delivery
            .finished_at
            .is_none_or(|finished| finished > minute_at)
}

fn summarize(
    pairs: &[&ForecastPair],
    values: impl Fn(&ForecastPair) -> (f64, f64),
) -> ErrorSummary {
    if pairs.is_empty() {
        return ErrorSummary::default();
    }
    let count = pairs.len() as f64;
    let mut sum = 0.0;
    let mut absolute = 0.0;
    let mut under = 0u64;
    let mut worst: Option<WorstMiss> = None;
    for pair in pairs {
        let (forecast, measured) = values(pair);
        let error = forecast - measured;
        sum += error;
        absolute += error.abs();
        if error < 0.0 {
            under += 1;
        }
        if worst
            .as_ref()
            .is_none_or(|worst| error.abs() > worst.error.abs())
        {
            worst = Some(WorstMiss {
                error,
                forecast,
                measured,
                forecast_at: pair.forecast_at,
                minute_at: pair.minute_at,
                workspace_id: pair.workspace_id.clone(),
                run_id: pair.run_id.clone(),
            });
        }
    }
    ErrorSummary {
        mean_error: Some(sum / count),
        mean_absolute_error: Some(absolute / count),
        under_rate: Some(under as f64 / count),
        worst_miss: worst,
    }
}

fn shadow_decisions(records: &[AdmissionForecastRecord]) -> Vec<DrainShadowDecisions> {
    #[derive(Default)]
    struct Drain {
        shadow_passes: u64,
        enforce_passes: u64,
        admitted: BTreeSet<String>,
        would_hold: BTreeSet<String>,
        would_add: BTreeSet<String>,
        pull_requests: u64,
        pull_sent: u64,
        pull_would_hold: u64,
    }
    let mut drains: BTreeMap<(&str, &str), Drain> = BTreeMap::new();
    for record in records {
        let drain = drains
            .entry((record.workspace_id.as_str(), record.run_id.as_str()))
            .or_default();
        if let Some(pull) = &record.pull {
            drain.pull_requests += 1;
            drain.pull_sent += u64::from(pull.sent);
            if record.mode == SHADOW {
                drain.shadow_passes += 1;
                drain.admitted.extend(record.admitted.iter().cloned());
                drain.pull_would_hold += u64::from(pull.forecast_no_headroom);
            } else {
                drain.enforce_passes += 1;
            }
            continue;
        }
        if record.mode != SHADOW {
            drain.enforce_passes += 1;
            continue;
        }
        drain.shadow_passes += 1;
        let fit: BTreeSet<&String> = record.fit_admitted.iter().collect();
        let admitted: BTreeSet<&String> = record.admitted.iter().collect();
        drain
            .admitted
            .extend(admitted.iter().map(|id| (*id).clone()));
        drain
            .would_hold
            .extend(admitted.difference(&fit).map(|id| (*id).clone()));
        drain
            .would_add
            .extend(fit.difference(&admitted).map(|id| (*id).clone()));
    }
    drains
        .into_iter()
        .map(|((workspace_id, run_id), drain)| DrainShadowDecisions {
            workspace_id: workspace_id.to_string(),
            run_id: run_id.to_string(),
            shadow_passes: drain.shadow_passes,
            enforce_passes: drain.enforce_passes,
            admitted: drain.admitted.len() as u64,
            would_hold: drain.would_hold.into_iter().collect(),
            would_add: drain.would_add.into_iter().collect(),
            pull_requests: drain.pull_requests,
            pull_sent: drain.pull_sent,
            pull_would_hold: drain.pull_would_hold,
        })
        .collect()
}
