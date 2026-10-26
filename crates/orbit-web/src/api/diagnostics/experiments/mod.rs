//! `/diagnostics/experiments/{id}/readout`: one experiment's readout, the
//! same JSON `orbit.experiment.readout` returns, for Analytics › Experiments.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use crate::state::Ws;

#[derive(Deserialize, Default)]
pub(in crate::api) struct ExperimentReadoutQuery {
    /// Report only this metric.
    #[serde(default)]
    metric: Option<String>,
}

pub(in crate::api) async fn diagnostics_experiment_readout(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(q): Query<ExperimentReadoutQuery>,
) -> Response {
    match super::super::blocking("experiment readout", move || {
        runtime.experiment_readout(&id, q.metric.as_deref())
    })
    .await
    {
        Ok(readout) => Json(readout).into_response(),
        Err(response) => *response,
    }
}
