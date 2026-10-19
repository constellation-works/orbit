use std::collections::BTreeMap;

use orbit_store::contracts::AdmissionForecastRecord;
use orbit_types::task::Task;
use orbit_types::workflow::{
    DrainAdmissionForecast, DrainAdmissionPass, DrainPassObservation, RunStateUpdate,
};
use serde_json::Value;

use super::super::drains::drain_run_id;
use crate::OrbitRuntime;
use crate::application::admission::gap_fit::task_lookup;
use crate::runtime::host_resource::forecast::ForecastPoints;

/// Record the pass's forecast, counting what it admitted, with its gap-fit
/// decisions, for the forecast accuracy report. Best effort, like the last
/// pass: a drain that cannot record it must still admit work.
pub(super) fn record_admission_forecast(
    runtime: &OrbitRuntime,
    input: &Value,
    mut forecast: ForecastPoints,
    record: &DrainAdmissionForecast,
    admitted: &[String],
    tasks: &BTreeMap<String, Task>,
) {
    let Some(run_id) = drain_run_id(input) else {
        return;
    };
    let result = runtime.workspace_id().and_then(|workspace_id| {
        for task in admitted.iter().filter_map(|task_id| tasks.get(task_id)) {
            forecast.admit(&task.id, &task_lookup(&workspace_id, task));
        }
        runtime.record_admission_forecast(&AdmissionForecastRecord {
            workspace_id,
            run_id: run_id.to_string(),
            forecast_at: forecast.at,
            mode: record.mode.clone(),
            cpu_count: forecast.cpu_count,
            memory_total_bytes: forecast.memory_total_bytes,
            baseline_cpu_cores: Some(forecast.baseline_cpu_cores),
            baseline_memory_bytes: Some(forecast.baseline_memory_bytes),
            covered_deliveries: Some(forecast.covered_deliveries.clone()),
            points: forecast.into_points(),
            admitted: admitted.to_vec(),
            pull: None,
            fit_admitted: record
                .admitted
                .iter()
                .map(|admission| admission.task_id.clone())
                .collect(),
            fit_skipped: record
                .skipped
                .iter()
                .map(|skip| skip.task_id.clone())
                .collect(),
        })
    });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            %error,
            "drain could not record its admission forecast; the accuracy report will miss this pass"
        );
    }
}

/// Persist what this pass left waiting on the drain's own run state, so
/// `orbit run show` can report it after the drain ends. Best effort: a run
/// that cannot record it must still admit work. Returns whether the pass
/// was persisted, so an external report can rely on its durable identity.
///
/// `observation`, when given, counts the pass in the drain's pass counters
/// in the same write; a rewrite of an already counted pass passes `None`.
pub(super) fn record_last_pass(
    runtime: &OrbitRuntime,
    input: &Value,
    pass: DrainAdmissionPass,
    observation: Option<DrainPassObservation>,
) -> bool {
    let Some(run_id) = drain_run_id(input) else {
        return false;
    };
    let mut pass = Some((pass, observation));
    match runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            if let Some((pass, observation)) = pass.take() {
                if let Some(observation) = observation {
                    state
                        .pull_drain
                        .count_admission_pass(&observation, pass.recorded_at);
                }
                state.pull_drain.record_admission_pass(pass);
            }
            Ok(())
        }) {
        Ok(RunStateUpdate::Updated) => true,
        Ok(RunStateUpdate::NotFound | RunStateUpdate::NoState) => false,
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                %error,
                "drain could not record its admission pass; run show will not list what it left waiting"
            );
            false
        }
    }
}
