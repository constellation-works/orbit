//! Tasks reaching a terminal status per bucket, for Health › Metrics.
//!
//! A task counts at its index `updated_at`, which is its move to the terminal
//! status unless a later write (a comment, an artifact) touched the bundle.

use std::collections::HashMap;

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// Complexities `done` reports on its own; the rest add up to `other`.
const DONE_SERIES: [&str; 3] = ["low", "medium", "hard"];

pub(in crate::api) async fn diagnostics_completions(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match trend_range(&q) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("completion trend", move || {
        completion_trend(&runtime, &range)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

#[derive(Clone, Default)]
struct Counts {
    done_by_complexity: HashMap<String, u64>,
    rejected: u64,
    archived: u64,
}

impl Counts {
    fn record(&mut self, status: TaskStatus, complexity: &str) {
        match status {
            TaskStatus::Done => {
                *self
                    .done_by_complexity
                    .entry(complexity.to_string())
                    .or_default() += 1;
            }
            TaskStatus::Rejected => self.rejected += 1,
            TaskStatus::Archived => self.archived += 1,
            _ => {}
        }
    }

    fn to_json(&self) -> Value {
        let total: u64 = self.done_by_complexity.values().sum();
        let mut done = json!({ "total": total });
        let mut listed = 0;
        for key in DONE_SERIES {
            let count = self.done_by_complexity.get(key).copied().unwrap_or(0);
            listed += count;
            done[key] = json!(count);
        }
        done["other"] = json!(total - listed);
        json!({ "done": done, "rejected": self.rejected, "archived": self.archived })
    }
}

fn completion_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
) -> Result<Value, orbit_core::OrbitError> {
    let transitions = runtime.task_terminal_transitions(TrendWindow::read_since(range))?;
    let window = TrendWindow::new(range, transitions.first().map(|row| row.at));
    let mut buckets = vec![Counts::default(); window.bucket_count()];
    let mut current = Counts::default();
    let mut previous = Counts::default();
    for row in &transitions {
        if let Some(index) = window.bucket_of(row.at) {
            buckets[index].record(row.status, &row.complexity);
            current.record(row.status, &row.complexity);
        } else if window.in_previous(row.at) {
            previous.record(row.status, &row.complexity);
        }
    }
    let buckets: Vec<Value> = buckets
        .iter()
        .enumerate()
        .map(|(index, counts)| {
            let mut row = counts.to_json();
            row["start"] = json!(window.bucket_start(index).to_rfc3339());
            row
        })
        .collect();
    Ok(json!({
        "window": window.to_json(),
        "buckets": buckets,
        "current": current.to_json(),
        "previous": window.previous.map(|_| previous.to_json()),
    }))
}
