use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::options::Options;
use super::{assembly, cohorts, history, persistence, projection, simulation};

pub(super) fn calculate_backlog_forecast(
    runtime: &OrbitRuntime,
    options: &Options,
    now: DateTime<Utc>,
) -> Result<Value, OrbitError> {
    let preparation_started = std::time::Instant::now();
    let mut loaded = history::load_forecast_history(runtime, options, now)?;
    let cohorts = cohorts::prepare_forecast_cohorts(runtime, &loaded, now)?;
    let prepared =
        assembly::assemble_forecast_simulation(runtime, options, &loaded, &cohorts, now)?;
    let preparation_ms = preparation_started.elapsed().as_secs_f64() * 1000.0;
    let simulation_started = std::time::Instant::now();
    let mut forecast = simulation::simulate_backlog_forecast(
        &prepared.items,
        &prepared.resolved,
        &cohorts.history,
        &cohorts.episodes,
        &prepared.admission,
        &prepared.plan,
        prepared.slots,
        &prepared.rework,
        options.arrivals.then_some(&prepared.arrival_rates),
    );
    let simulation_ms = simulation_started.elapsed().as_secs_f64() * 1000.0;
    let items = &prepared.items;
    let rework = &prepared.rework;
    for scenario in std::iter::once(&mut forecast.backlog).chain(forecast.if_approved.iter_mut()) {
        if let Some(predictions) = scenario["task_predictions"].as_array_mut() {
            for prediction in predictions {
                if let Some(index) = items
                    .iter()
                    .position(|item| prediction["id"] == item.task_id)
                {
                    prediction["riskiness"] = rework.risks[index].clone();
                    prediction["regression_probability"] = json!(rework.probabilities[index]);
                    prediction["regression_probability_basis"] = json!(rework.prior_basis[index]);
                    prediction["regression_bundle_samples"] = json!(rework.pools[index].len());
                    prediction["regression_bundle_basis"] = json!(rework.pool_basis[index]);
                }
            }
        }
    }
    let stored = std::mem::take(&mut loaded.stored);
    let context = projection::ForecastProjection {
        runtime,
        options,
        loaded: &loaded,
        cohorts: &cohorts,
        prepared: &prepared,
        forecast: &forecast,
        now,
    };
    let inputs = projection::forecast_inputs(&context);
    persistence::persist_forecast(&context, inputs, stored, preparation_ms, simulation_ms)
}
