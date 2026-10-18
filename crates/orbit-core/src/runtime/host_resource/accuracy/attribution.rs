//! Align worker minutes to host minutes once, then select only deliveries
//! priced by each forecast. This uses raw measurements, not profile eligibility.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, DurationRound, Utc};
use orbit_store::contracts::{AdmissionForecastRecord, DeliveryRunResources};

use crate::runtime::host_resource::RESOURCE_SAMPLE_INTERVAL;

struct RunLoad<'a> {
    run: &'a DeliveryRunResources,
    minutes: BTreeMap<DateTime<Utc>, (f64, f64)>,
}

pub(super) struct Attribution<'a> {
    tasks: BTreeMap<(&'a str, &'a str), Vec<RunLoad<'a>>>,
}

impl<'a> Attribution<'a> {
    pub(super) fn new(resources: &'a [DeliveryRunResources]) -> Self {
        let mut tasks: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for run in resources {
            let mut minutes: BTreeMap<_, (f64, f64)> = BTreeMap::new();
            for (index, minute) in run.minutes.iter().enumerate() {
                let start = minute.minute_started_at;
                let sampled = Duration::seconds(
                    i64::from(minute.samples) * RESOURCE_SAMPLE_INTERVAL.as_secs() as i64,
                )
                .min(Duration::minutes(1));
                let mut end = start + sampled;
                if let Some(finished) = run.finished_at {
                    end = end.min(finished);
                }
                if let Some(next) = run.minutes.get(index + 1) {
                    end = end.min(next.minute_started_at);
                }
                let mut at = start.duration_trunc(Duration::minutes(1)).unwrap_or(start);
                while at < end {
                    let overlap = (end.min(at + Duration::minutes(1)) - start.max(at))
                        .num_milliseconds()
                        .max(0) as f64
                        / 60_000.0;
                    let load = minutes.entry(at).or_default();
                    load.0 += minute.cpu_cores * overlap;
                    load.1 += minute.memory_bytes as f64 * overlap;
                    at += Duration::minutes(1);
                }
            }
            tasks
                .entry((run.workspace_id.as_str(), run.task_id.as_str()))
                .or_default()
                .push(RunLoad { run, minutes });
        }
        Self { tasks }
    }

    pub(super) fn measured(
        &self,
        record: &AdmissionForecastRecord,
        minute_at: DateTime<Utc>,
    ) -> Option<(f64, f64)> {
        let mut load = (
            record.baseline_cpu_cores?,
            record.baseline_memory_bytes? as f64,
        );
        let mut counted = BTreeSet::new();
        for delivery in record.covered_deliveries.as_ref()? {
            let Some(runs) = self
                .tasks
                .get(&(delivery.workspace_id.as_str(), delivery.task_id.as_str()))
            else {
                continue;
            };
            // A reservation has no run yet. Resolve its first measured admission
            // once; a later re-admission of the same task is outside this forecast.
            let admitted_at = if delivery.run_ids.is_empty() {
                runs.iter()
                    .map(|run| run.run.delivery_admitted_at)
                    .filter(|at| at.timestamp_micros() >= delivery.admitted_at.timestamp_micros())
                    .min()
            } else {
                // Run-resource timestamps are persisted at microsecond precision;
                // captured admission metadata can still contain nanoseconds.
                Some(delivery.admitted_at)
            };
            for run in runs {
                if (Some(run.run.delivery_admitted_at.timestamp_micros())
                    == admitted_at.map(|at| at.timestamp_micros())
                    || delivery.run_ids.contains(&run.run.run_id))
                    && counted.insert((run.run.workspace_id.as_str(), run.run.run_id.as_str()))
                    && let Some(measured) = run.minutes.get(&minute_at)
                {
                    load.0 += measured.0;
                    load.1 += measured.1;
                }
            }
        }
        Some(load)
    }
}
