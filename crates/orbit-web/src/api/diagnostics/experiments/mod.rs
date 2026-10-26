//! Analytics › Experiments: `/diagnostics/experiments` lists the workspace's
//! experiments with each arm's enrollment, `/diagnostics/experiments/{id}`
//! shows one with its event timeline, and `/diagnostics/experiments/{id}/readout`
//! returns the JSON `orbit.experiment.readout` returns plus `cumulative`, the
//! primary metric's per-arm cumulative series on the shared trend ladder.

mod cumulative;

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

use crate::state::Ws;

#[derive(Deserialize, Default)]
pub(in crate::api) struct ExperimentReadoutQuery {
    /// Report only this metric.
    #[serde(default)]
    metric: Option<String>,
}

pub(in crate::api) async fn diagnostics_experiments(Ws(runtime): Ws) -> Response {
    match super::super::blocking("experiment list", move || runtime.experiment_progress()).await {
        Ok(experiments) => Json(json!({ "experiments": experiments })).into_response(),
        Err(response) => *response,
    }
}

pub(in crate::api) async fn diagnostics_experiment(
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    match super::super::blocking("experiment show", move || runtime.experiment_view(&id)).await {
        Ok(view) => Json(view).into_response(),
        Err(response) => *response,
    }
}

pub(in crate::api) async fn diagnostics_experiment_readout(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<ExperimentReadoutQuery>,
) -> Response {
    match super::super::blocking("experiment readout", move || {
        let readout = runtime.experiment_readout(&id, q.metric.as_deref())?;
        let cumulative = cumulative::primary_cumulative(&readout);
        let mut body = serde_json::to_value(&readout).map_err(|error| {
            orbit_core::OrbitError::Execution(format!("experiment readout is not JSON: {error}"))
        })?;
        body["cumulative"] = cumulative;
        Ok(body)
    })
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}
