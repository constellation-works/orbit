//! The primary metric's cumulative per-arm series: at the end of each bucket
//! of the shared trend ladder, every arm's mean (a rate for a 0/1 metric)
//! over the settled tasks assigned by then. The window opens when the
//! experiment started, or at its first assignment, and closes now.

use chrono::{DateTime, Utc};
use orbit_core::application::experiment::ExperimentReadout;
use serde_json::{Value, json};

use super::super::metrics::DiagnosticsRange;
use super::super::trend::TrendWindow;

pub(super) fn primary_cumulative(readout: &ExperimentReadout) -> Value {
    let observations = &readout.primary_observations;
    let started = readout
        .experiment
        .started_at
        .as_deref()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc));
    let first = observations
        .first()
        .map(|observation| observation.assigned_at);
    let earliest = match (started, first) {
        (Some(started), Some(first)) => Some(started.min(first)),
        (started, first) => started.or(first),
    };
    let range = DiagnosticsRange {
        key: "all".to_string(),
        since: None,
        until: Utc::now(),
    };
    let window = TrendWindow::new(&range, earliest);
    let buckets = window.bucket_count();
    let arms: Vec<Value> = readout
        .arms
        .iter()
        .map(|arm| {
            let mut sums = vec![(0usize, 0.0f64); buckets];
            for observation in observations.iter().filter(|o| o.arm == arm.name) {
                // An assignment before the window (clock skew) counts from
                // the first bucket.
                let index = window.bucket_of(observation.assigned_at).unwrap_or(
                    if observation.assigned_at < window.start {
                        0
                    } else {
                        buckets - 1
                    },
                );
                sums[index].0 += 1;
                sums[index].1 += observation.value;
            }
            let (mut n, mut total) = (0usize, 0.0f64);
            let points: Vec<Value> = sums
                .iter()
                .enumerate()
                .map(|(index, (count, sum))| {
                    n += count;
                    total += sum;
                    json!({
                        "start": window.bucket_start(index).to_rfc3339(),
                        "n": n,
                        "value": (n > 0).then(|| total / n as f64),
                    })
                })
                .collect();
            json!({ "arm": arm.name, "control": arm.control, "points": points })
        })
        .collect();
    json!({
        "metric": readout.experiment.primary_metric,
        "window": window.to_json(),
        "arms": arms,
    })
}
