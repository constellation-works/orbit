//! Pair forecasts with measured minutes and classify missing observations.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, DurationRound, Utc};
use orbit_store::contracts::{
    AdmissionForecastRecord, DeliveryRunInterval, DeliveryRunResources, HostResourceMinute,
};

use super::attribution;
use super::shadow::shadow_decisions;
use super::{
    AccuracySelection, ErrorSummary, ForecastAccuracy, ForecastPair, OffsetAccuracy, WorstMiss,
};
use crate::runtime::host_resource::forecast::ACCURACY_OFFSETS;

/// The report over preselected `records` against the host's measured `minutes`.
///
/// `deliveries` are the workers that could have sampled the host. An ended
/// minute with no row is a `gap` when one of them was running for the whole
/// minute, `partial` when one overlapped it but none saw it whole, and
/// `unsampled` otherwise. A drain that only recorded forecasts is not a
/// delivery.
pub(crate) fn forecast_accuracy(
    records: &[AdmissionForecastRecord],
    minutes: &[HostResourceMinute],
    deliveries: &[DeliveryRunInterval],
    resources: &[DeliveryRunResources],
    selection: AccuracySelection,
    report_at: DateTime<Utc>,
) -> ForecastAccuracy {
    let attribution = attribution::Attribution::new(resources);
    let measured: BTreeMap<DateTime<Utc>, &HostResourceMinute> = minutes
        .iter()
        .map(|minute| (minute.minute_at, minute))
        .collect();
    let mut pairs = Vec::new();
    let mut pending: BTreeMap<u32, u64> = BTreeMap::new();
    let mut unsampled: BTreeMap<u32, u64> = BTreeMap::new();
    let mut partial: BTreeMap<u32, u64> = BTreeMap::new();
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
                let bucket = match classify_missing(deliveries, minute_at) {
                    MissingMinute::Gap => &mut gap,
                    MissingMinute::Partial => &mut partial,
                    MissingMinute::Unsampled => &mut unsampled,
                };
                *bucket.entry(point.offset_minutes).or_default() += 1;
                continue;
            };
            let attributed = attribution.measured(record, minute_at);
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
                attributed_cpu_cores: attributed.map(|load| load.0),
                attributed_memory_bytes: attributed.map(|load| load.1),
            });
        }
    }

    let mut offsets: BTreeSet<u32> = ACCURACY_OFFSETS.into_iter().collect();
    offsets.extend(pairs.iter().map(|pair| pair.offset_minutes));
    offsets.extend(pending.keys().copied());
    offsets.extend(unsampled.keys().copied());
    offsets.extend(partial.keys().copied());
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
            let partial = partial.get(&offset).copied().unwrap_or(0);
            let gap = gap.get(&offset).copied().unwrap_or(0);
            let missing = unsampled + partial + gap;
            let attributed: Vec<&ForecastPair> = at_offset
                .iter()
                .copied()
                .filter(|pair| {
                    pair.attributed_cpu_cores.is_some() && pair.attributed_memory_bytes.is_some()
                })
                .collect();
            OffsetAccuracy {
                offset_minutes: offset,
                paired: at_offset.len() as u64,
                unmeasured: pending + missing,
                pending,
                missing,
                unsampled,
                partial,
                gap,
                attributed_paired: attributed.len() as u64,
                attributed_cpu: summarize(&attributed, |pair| {
                    (
                        pair.forecast_cpu_cores,
                        pair.attributed_cpu_cores.unwrap_or_default(),
                    )
                }),
                attributed_memory: summarize(&attributed, |pair| {
                    (
                        pair.forecast_memory_bytes as f64,
                        pair.attributed_memory_bytes.unwrap_or_default(),
                    )
                }),
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
        limiters: Vec::new(),
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

/// An ended minute with no host row.
enum MissingMinute {
    /// No delivery overlapped the minute.
    Unsampled,
    /// A delivery overlapped the minute, but none saw it whole.
    Partial,
    /// A delivery was running for the whole minute.
    Gap,
}

/// Classify an ended minute that has no host row.
///
/// A worker can offer a host row only for a minute it saw whole: it was
/// already running before the minute began, and it was still running when
/// the minute ended. The minute it starts in, and the minute it finishes
/// in, stay unrecorded on purpose.
fn classify_missing(deliveries: &[DeliveryRunInterval], minute_at: DateTime<Utc>) -> MissingMinute {
    let end = minute_at + Duration::minutes(1);
    let mut overlapped = false;
    for delivery in deliveries {
        if !delivery_covers(delivery, minute_at) {
            continue;
        }
        if delivery_observed_whole(delivery, minute_at, end) {
            return MissingMinute::Gap;
        }
        overlapped = true;
    }
    if overlapped {
        MissingMinute::Partial
    } else {
        MissingMinute::Unsampled
    }
}

/// Whether `delivery` was already running before `minute_at` and was still
/// running at `end`, so the sampler could have recorded that minute whole.
fn delivery_observed_whole(
    delivery: &DeliveryRunInterval,
    minute_at: DateTime<Utc>,
    end: DateTime<Utc>,
) -> bool {
    delivery.started_at < minute_at && delivery.finished_at.is_none_or(|finished| finished >= end)
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
