//! Implement-one duration aggregation by actor and task complexity.

use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use std::collections::HashMap;

use super::super::map_runtime_error;
use super::metrics::actor_label;
use crate::state::Ws;

pub(in crate::api) async fn diagnostics_implement_one(Ws(runtime): Ws) -> Response {
    let runtime_clone = runtime.clone();
    let aggregation =
        match tokio::task::spawn_blocking(move || compute_implement_one_by_actor(&runtime_clone))
            .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return map_runtime_error(e),
            Err(join_err) => {
                return map_runtime_error(orbit_core::OrbitError::Execution(format!(
                    "implement_one aggregation panicked: {join_err}"
                )));
            }
        };

    Json(json!({
        "implement_one_by_actor": aggregation.by_actor,
        "implement_one_by_complexity": aggregation.by_complexity,
    }))
    .into_response()
}

fn compute_implement_one_by_actor(
    runtime: &OrbitRuntime,
) -> Result<ImplementOneAggregation, orbit_core::OrbitError> {
    let since = chrono::Utc::now() - chrono::Duration::days(30);

    let records = runtime.invocation_records(orbit_core::InvocationQuery {
        since: Some(since),
        until: None,
        activity_id: Some("implement_one".to_string()),
        limit: 100_000,
        ..Default::default()
    })?;
    let complexity_by_task = runtime.task_complexity_by_id()?;

    let mut durations_by_actor: HashMap<String, Vec<i64>> = HashMap::new();
    let mut durations_by_complexity_actor: HashMap<String, HashMap<String, Vec<i64>>> =
        HashMap::new();
    for record in records {
        let actor = actor_label(&record.agent, record.model.as_deref());
        let complexity = record
            .task_ids
            .first()
            .and_then(|task_id| complexity_by_task.get(task_id).cloned())
            .unwrap_or_else(|| orbit_types::task::UNSET_BUCKET.to_string());
        durations_by_actor
            .entry(actor.clone())
            .or_default()
            .push(record.duration_ms as i64);
        durations_by_complexity_actor
            .entry(complexity)
            .or_default()
            .entry(actor)
            .or_default()
            .push(record.duration_ms as i64);
    }

    let by_actor = actor_duration_rows(durations_by_actor);
    let mut by_complexity: Vec<Value> = durations_by_complexity_actor
        .into_iter()
        .map(|(complexity, actors)| {
            let actor_rows = actor_duration_rows(actors);
            let n: usize = actor_rows
                .iter()
                .map(|row| row["n"].as_u64().unwrap_or(0) as usize)
                .sum();
            json!({
                "complexity": complexity,
                "n": n,
                "actors": actor_rows,
            })
        })
        .collect();
    by_complexity.sort_by(|left, right| {
        orbit_types::task::complexity_bucket_ord(
            left["complexity"]
                .as_str()
                .unwrap_or(orbit_types::task::UNSET_BUCKET),
        )
        .cmp(&orbit_types::task::complexity_bucket_ord(
            right["complexity"]
                .as_str()
                .unwrap_or(orbit_types::task::UNSET_BUCKET),
        ))
    });

    Ok(ImplementOneAggregation {
        by_actor,
        by_complexity,
    })
}

struct ImplementOneAggregation {
    by_actor: Vec<Value>,
    by_complexity: Vec<Value>,
}

fn actor_duration_rows(durations_by_actor: HashMap<String, Vec<i64>>) -> Vec<Value> {
    let mut actor_vec: Vec<_> = durations_by_actor
        .into_iter()
        .map(|(actor, mut durations)| {
            durations.sort_unstable();
            let n = durations.len();
            let avg = if n > 0 {
                durations.iter().sum::<i64>() as f64 / n as f64
            } else {
                0.0
            };
            let p50_idx = ((n as f64 * 0.50).ceil() as usize).min(n).saturating_sub(1);
            let p50 = if n > 0 { durations[p50_idx] } else { 0 };
            let p95_idx = ((n as f64 * 0.95).ceil() as usize).min(n).saturating_sub(1);
            let p95 = if n > 0 { durations[p95_idx] } else { 0 };
            json!({
                "actor": actor,
                "n": n,
                "avg": avg,
                "p50": p50,
                "p95": p95,
            })
        })
        .collect();

    actor_vec.sort_by(|a, b| {
        b["avg"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["avg"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    actor_vec
}
