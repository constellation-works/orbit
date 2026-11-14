//! Tasks reaching a terminal status per bucket, for Analytics › Metrics.
//!
//! A task counts at the latest entry into its current terminal status in its
//! durable history, so a later comment, artifact or crew backfill cannot move
//! it. A task with no such entry has no known completion time: it stays out of
//! every bucket and total, and `undated` / `previous_undated` count it instead
//! when its last write leaves that window possible.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use std::sync::Arc;

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::runtime_memo::DASHBOARD_POLL_MEMO_TTL;
use crate::state::{DashboardState, Ws};

/// Complexities `done` reports on its own; the rest add up to `other`.
const DONE_SERIES: [&str; 3] = ["low", "medium", "hard"];

pub(in crate::api) async fn diagnostics_completions(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match trend_range(&q) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    let key = ("completions".to_string(), range.key.clone());
    let compute_runtime = Arc::clone(&runtime);
    match state
        .metrics_trends_memo()
        .get_or_compute(&runtime, key, DASHBOARD_POLL_MEMO_TTL, move || {
            completion_trend(&compute_runtime, &range)
        })
        .await
    {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(error) => map_runtime_error(error),
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
    let dates = runtime.task_terminal_dates(TrendWindow::read_since_with_day_lead(range))?;
    let window = TrendWindow::with_day_lead(range, dates.dated.first().map(|row| row.terminal_at));
    let mut buckets = vec![Counts::default(); window.bucket_count()];
    let mut lead = vec![Counts::default(); window.lead_count()];
    let mut current = Counts::default();
    let mut previous = Counts::default();
    for row in &dates.dated {
        let (at, task) = (row.terminal_at, &row.task);
        if let Some(index) = window.bucket_of(at) {
            buckets[index].record(task.status, &task.complexity);
            current.record(task.status, &task.complexity);
        } else if window.in_previous(at) {
            previous.record(task.status, &task.complexity);
        }
        if let Some(index) = window.lead_bucket_of(at) {
            lead[index].record(task.status, &task.complexity);
        }
    }
    let mut undated = Counts::default();
    let mut previous_undated = Counts::default();
    for task in &dates.undated {
        if window.may_hold_undated(task.at) {
            undated.record(task.status, &task.complexity);
        }
        if window.previous_may_hold_undated(task.at) {
            previous_undated.record(task.status, &task.complexity);
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
        "lead_buckets": lead.iter().map(Counts::to_json).collect::<Vec<_>>(),
        "current": current.to_json(),
        "previous": window.previous.map(|_| previous.to_json()),
        "undated": undated.to_json(),
        "previous_undated": window.previous.map(|_| previous_undated.to_json()),
    }))
}
