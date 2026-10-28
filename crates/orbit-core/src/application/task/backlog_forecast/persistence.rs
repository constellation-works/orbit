use std::collections::HashMap;

use chrono::Duration;
use orbit_common::OrbitError;
use orbit_store::contracts::StoredBacklogForecast;
use serde_json::{Value, json};

use super::{model, projection, scoring};

/// At most one stored forecast per workspace per this many minutes.
pub(super) const STORE_INTERVAL_MINUTES: i64 = 60;
/// Stored forecasts the scoring pass reads, newest first.
pub(super) const SCORED_FORECASTS: usize = 60;

pub(super) fn persist_forecast(
    context: &projection::ForecastProjection,
    inputs: &Value,
    mut stored: Vec<StoredBacklogForecast>,
) -> Result<(Option<bool>, Vec<StoredBacklogForecast>, Value), OrbitError> {
    let runtime = context.runtime;
    let options = context.options;
    let now = context.now;
    let tasks = &context.loaded.tasks;
    let done_at = &context.loaded.done_at;
    let task_episodes = &context.loaded.task_episodes;
    let runs = &context.cohorts.runs;
    let history = &context.cohorts.history;
    let episodes = &context.cohorts.episodes;
    let items = &context.prepared.items;
    let resolved = &context.prepared.resolved;
    let rework = &context.prepared.rework;
    let plan = &context.prepared.plan;
    let admission = &context.prepared.admission;
    let forecast = context.forecast;

    let mut stored_now = None;
    let backlog_count = items.iter().filter(|item| !item.proposed).count();
    if options.canonical() && backlog_count > 0 && !history.is_empty() {
        let predictions: HashMap<&str, &Value> = forecast.backlog["task_predictions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row["id"].as_str()?, row)))
            .collect();
        let stored_tasks: Vec<Value> = items
            .iter()
            .zip(resolved)
            .enumerate()
            .filter(|(_, (item, _))| !item.proposed)
            .map(|(index, (item, resolved))| {
                let prediction = predictions.get(item.task_id.as_str());
                let field = |key: &str| prediction.map_or(Value::Null, |row| row[key].clone());
                json!({
                    "id": item.task_id,
                    "complexity": item.complexity,
                    "in_progress": item.in_progress,
                    "status": if item.in_progress {
                        "in-progress"
                    } else if item.blocked {
                        "blocked"
                    } else {
                        "backlog"
                    },
                    "basis": resolved.basis.key(),
                    "outcome_basis": resolved.outcome_basis.key(),
                    "setback_probability": resolved.setback_probability(episodes),
                    "return_basis": resolved.return_basis.key(),
                    "return_probability": resolved.returns.return_probability(),
                    "riskiness": rework.risks[index],
                    "regression_probability": rework.probabilities[index],
                    "regression_probability_basis": rework.prior_basis[index],
                    "regression_bundle_samples": rework.pools[index].len(),
                    "regression_bundle_basis": rework.pool_basis[index],
                    "cell_samples": resolved.cell_samples,
                    "review_mode": item.review_mode,
                    "review_mode_source": item.review_mode_source,
                    "review_mode_fallback": resolved.review_mode_fallback,
                    // Every simulated attempt's use, not one delivery
                    // draw [ORB-18859]: unknown when it was not scheduled.
                    "forecast": {
                        "work_hours": field("work_hours"),
                        "tokens": field("tokens"),
                        "usd": field("usd"),
                    },
                    "dependencies": plan.edges[index]
                        .iter()
                        .map(|dep| items[*dep].task_id.as_str())
                        .collect::<Vec<_>>(),
                    "footprint": admission.surface_hash(index),
                    "profile_source": admission.profile_source(index).map(|source| source.as_str()),
                    "unforecastable": plan.backlog[index],
                    "predicted_start_hours": field("predicted_start_hours"),
                    "predicted_start_rank": field("predicted_start_rank"),
                    "completion_probability": field("completion_probability"),
                })
            })
            .collect();
        // Per-task predictions are stored once, with each task above.
        let mut result = forecast.backlog.clone();
        if let Some(result) = result.as_object_mut() {
            result.remove("task_predictions");
        }
        let row = StoredBacklogForecast {
            workspace_id: String::new(),
            forecast_at: now,
            model_version: model::MODEL_VERSION,
            inputs_json: inputs.to_string(),
            result_json: result.to_string(),
            tasks_json: Value::Array(stored_tasks).to_string(),
        };
        match runtime.record_backlog_forecast(row, Duration::minutes(STORE_INTERVAL_MINUTES)) {
            Ok(written) => stored_now = Some(written),
            Err(error) => tracing::warn!(%error, "backlog forecast: not stored"),
        }
    }
    if stored_now == Some(true) {
        for row in runtime.backlog_forecasts(SCORED_FORECASTS)? {
            if !stored
                .iter()
                .any(|existing| existing.forecast_at == row.forecast_at)
            {
                stored.push(row);
            }
        }
        stored.sort_by_key(|row| std::cmp::Reverse(row.forecast_at));
    }
    let calibration = scoring::score_backlog_forecasts(
        &stored,
        tasks,
        done_at,
        runs,
        task_episodes,
        model::MODEL_VERSION,
        now,
    );

    Ok((stored_now, stored, calibration))
}
