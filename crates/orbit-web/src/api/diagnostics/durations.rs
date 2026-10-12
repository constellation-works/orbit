//! implement_one durations for Health › Metrics: the windowed trend by
//! complexity and actor, and the 30-day actor table per complexity.

use std::collections::{BTreeMap, HashMap};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Utc};
use orbit_core::{InvocationQuery, OrbitRuntime};
use orbit_types::task::{UNSET_BUCKET, complexity_bucket_ord};
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::{DiagnosticsRange, actor_label};
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

const ACTIVITY: &str = "implement_one";
/// Rows read per request; reaching it sets `truncated` on the response.
const RECORD_LIMIT: usize = 100_000;
/// The per-complexity actor table needs more runs than a short window holds.
const COMPLEXITY_ACTORS_DAYS: i64 = 30;
const SLOWEST_RUNS: usize = 8;
/// Complexities a bucket reports on its own; the rest count toward `all` only.
const BUCKET_SERIES: [&str; 3] = ["low", "medium", "hard"];
/// Complexities the window totals report, `all` aside.
const TOTAL_SERIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];

pub(in crate::api) async fn diagnostics_implement_one(
    Ws(runtime): Ws,
    Query(q): Query<DiagnosticsQuery>,
) -> Response {
    let range = match trend_range(&q) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("implement_one aggregation", move || {
        implement_one_trend(&runtime, &range)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

struct Run {
    at: DateTime<Utc>,
    actor: String,
    complexity: String,
    duration_ms: u64,
    task_id: Option<String>,
}

fn implement_one_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
) -> Result<Value, orbit_core::OrbitError> {
    let complexity_since = Utc::now() - Duration::days(COMPLEXITY_ACTORS_DAYS);
    let records = runtime.workspace_invocation_records(InvocationQuery {
        since: TrendWindow::read_since(range).map(|since| since.min(complexity_since)),
        activity_id: Some(ACTIVITY.to_string()),
        limit: RECORD_LIMIT,
        ..Default::default()
    })?;
    let truncated = records.len() >= RECORD_LIMIT;
    let complexity_by_task = runtime.task_complexity_by_id()?;
    let runs: Vec<Run> = records
        .into_iter()
        .map(|record| Run {
            at: record.ts,
            actor: actor_label(&record.agent, record.model.as_deref()),
            complexity: record
                .task_ids
                .first()
                .and_then(|task_id| complexity_by_task.get(task_id).cloned())
                .unwrap_or_else(|| UNSET_BUCKET.to_string()),
            duration_ms: record.duration_ms,
            task_id: record.task_ids.first().cloned(),
        })
        .collect();

    let window = TrendWindow::new(range, runs.iter().map(|run| run.at).min());
    let bucket_count = window.bucket_count();
    let mut buckets: Vec<HashMap<&str, Vec<u64>>> = vec![HashMap::new(); bucket_count];
    let mut current: HashMap<&str, Vec<u64>> = HashMap::new();
    let mut previous: HashMap<&str, Vec<u64>> = HashMap::new();
    let mut actor_buckets: BTreeMap<&str, Vec<Vec<u64>>> = BTreeMap::new();
    let mut actor_previous: HashMap<&str, Vec<u64>> = HashMap::new();
    let mut slowest: Vec<&Run> = Vec::new();
    let mut complexity_actors: HashMap<&str, HashMap<&str, Vec<u64>>> = HashMap::new();

    for run in &runs {
        if run.at >= complexity_since {
            complexity_actors
                .entry(&run.complexity)
                .or_default()
                .entry(&run.actor)
                .or_default()
                .push(run.duration_ms);
        }
        if let Some(index) = window.bucket_of(run.at) {
            for series in ["all", run.complexity.as_str()] {
                buckets[index]
                    .entry(series)
                    .or_default()
                    .push(run.duration_ms);
                current.entry(series).or_default().push(run.duration_ms);
            }
            actor_buckets
                .entry(&run.actor)
                .or_insert_with(|| vec![Vec::new(); bucket_count])[index]
                .push(run.duration_ms);
            slowest.push(run);
        } else if window.in_previous(run.at) {
            for series in ["all", run.complexity.as_str()] {
                previous.entry(series).or_default().push(run.duration_ms);
            }
            actor_previous
                .entry(&run.actor)
                .or_default()
                .push(run.duration_ms);
        }
    }

    let buckets: Vec<Value> = buckets
        .into_iter()
        .enumerate()
        .map(|(index, mut series)| {
            let mut row = json!({ "start": window.bucket_start(index).to_rfc3339() });
            let all = series.remove("all");
            let listed: usize = BUCKET_SERIES
                .iter()
                .map(|key| series.get(key).map_or(0, Vec::len))
                .sum();
            row["other_runs"] = json!(all.as_ref().map_or(0, Vec::len) - listed);
            row["all"] = duration_stats(all);
            for key in BUCKET_SERIES {
                row[key] = duration_stats(series.remove(key));
            }
            row
        })
        .collect();

    let mut actors: Vec<(usize, Value)> = actor_buckets
        .into_iter()
        .map(|(actor, per_bucket)| {
            let trend: Vec<Value> = per_bucket
                .iter()
                .map(|durations| duration_stats(Some(durations.clone()))["avg"].clone())
                .collect();
            let durations: Vec<u64> = per_bucket.into_iter().flatten().collect();
            let n = durations.len();
            let mut row = duration_stats(Some(durations));
            row["actor"] = json!(actor);
            row["previous_avg"] = duration_stats(actor_previous.remove(actor))["avg"].clone();
            row["trend"] = json!(trend);
            (n, row)
        })
        .collect();
    actors.sort_by(|(left_n, left), (right_n, right)| {
        right_n
            .cmp(left_n)
            .then_with(|| left["actor"].as_str().cmp(&right["actor"].as_str()))
    });

    slowest.sort_by(|left, right| {
        right
            .duration_ms
            .cmp(&left.duration_ms)
            .then_with(|| right.at.cmp(&left.at))
    });
    let slowest: Vec<Value> = slowest
        .into_iter()
        .take(SLOWEST_RUNS)
        .map(|run| {
            json!({
                "ts": run.at.to_rfc3339(),
                "actor": run.actor,
                "complexity": run.complexity,
                "duration_ms": run.duration_ms,
                "task_id": run.task_id,
            })
        })
        .collect();

    Ok(json!({
        "window": window.to_json(),
        "truncated": truncated,
        "buckets": buckets,
        "current": series_totals(current),
        "previous": window.previous.map(|_| series_totals(previous)),
        "actors": actors.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
        "slowest": slowest,
        "implement_one_by_complexity": by_complexity_rows(complexity_actors),
    }))
}

/// `all` plus every [`TOTAL_SERIES`] complexity, each `null` without runs.
fn series_totals(mut series: HashMap<&str, Vec<u64>>) -> Value {
    let mut totals = json!({ "all": duration_stats(series.remove("all")) });
    for key in TOTAL_SERIES {
        totals[key] = duration_stats(series.remove(key));
    }
    totals
}

/// `{n, avg, p50, p95}` in milliseconds, or `null` for no runs. Percentiles
/// are nearest-rank: the `ceil(n * p)`-th smallest duration.
fn duration_stats(durations: Option<Vec<u64>>) -> Value {
    let Some(mut durations) = durations.filter(|durations| !durations.is_empty()) else {
        return Value::Null;
    };
    durations.sort_unstable();
    let n = durations.len();
    let rank = |p: f64| durations[((n as f64 * p).ceil() as usize).clamp(1, n) - 1];
    json!({
        "n": n,
        "avg": durations.iter().sum::<u64>() as f64 / n as f64,
        "p50": rank(0.50),
        "p95": rank(0.95),
    })
}

fn by_complexity_rows(complexity_actors: HashMap<&str, HashMap<&str, Vec<u64>>>) -> Vec<Value> {
    let mut rows: Vec<(&str, Value)> = complexity_actors
        .into_iter()
        .map(|(complexity, actors)| {
            let mut actor_rows: Vec<Value> = actors
                .into_iter()
                .map(|(actor, durations)| {
                    let mut row = duration_stats(Some(durations));
                    row["actor"] = json!(actor);
                    row
                })
                .collect();
            actor_rows.sort_by(|left, right| {
                right["avg"]
                    .as_f64()
                    .partial_cmp(&left["avg"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let n: u64 = actor_rows
                .iter()
                .map(|row| row["n"].as_u64().unwrap_or(0))
                .sum();
            (
                complexity,
                json!({ "complexity": complexity, "n": n, "actors": actor_rows }),
            )
        })
        .collect();
    rows.sort_by(|(left, _), (right, _)| {
        complexity_bucket_ord(left).cmp(&complexity_bucket_ord(right))
    });
    rows.into_iter().map(|(_, row)| row).collect()
}
