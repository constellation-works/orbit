//! Analytics › Experiments: `/diagnostics/experiments` lists the workspace's
//! experiments with each arm's enrollment, `/diagnostics/experiments/{id}`
//! shows one with its event timeline, `/diagnostics/experiments/{id}/readout`
//! returns the JSON `orbit.experiment.readout` returns plus `cumulative`, and
//! `POST /diagnostics/experiments/{id}/conclude` records an operator's decision
//! through the same `change_experiment` path as the CLI. The list and the
//! detail carry `authoring` for the operator's create, edit and start routes
//! in [`authoring`].

mod authoring;
mod cumulative;

pub(in crate::api) use authoring::{
    diagnostics_experiment_create, diagnostics_experiment_start, diagnostics_experiment_update,
};

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_EXPERIMENT_CONCLUDE;
use orbit_core::application::experiment::{ExperimentActor, ExperimentChange};
use serde::Deserialize;
use serde_json::json;

use crate::state::{DashboardState, Ws};

#[derive(Deserialize, Default)]
pub(in crate::api) struct ExperimentReadoutQuery {
    /// Report only this metric.
    #[serde(default)]
    metric: Option<String>,
}

pub(in crate::api) async fn diagnostics_experiments(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
) -> Response {
    let operator_session = state.operator_session();
    match super::super::blocking("experiment list", move || {
        let experiments = runtime.experiment_progress()?;
        let authoring = authoring::experiment_authoring(&runtime, operator_session);
        Ok(json!({ "experiments": experiments, "authoring": authoring }))
    })
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}

pub(in crate::api) async fn diagnostics_experiment(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let conclude_authorized = super::super::routines::authorized_caller(
        &DASHBOARD_EXPERIMENT_CONCLUDE,
        state.operator_session(),
    )
    .is_ok();
    let operator_session = state.operator_session();
    match super::super::blocking("experiment show", move || {
        let view = runtime.experiment_view(&id)?;
        let mut body = serde_json::to_value(&view).map_err(|error| {
            orbit_core::OrbitError::Execution(format!("experiment view is not JSON: {error}"))
        })?;
        body["conclude_authorized"] = json!(conclude_authorized);
        body["authoring"] = authoring::experiment_authoring(&runtime, operator_session);
        Ok(body)
    })
    .await
    {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
pub(in crate::api) struct ConcludeBody {
    #[serde(default)]
    arm: Option<String>,
    reason: String,
}

/// `POST /diagnostics/experiments/{id}/conclude` — operator-only conclusion.
/// Authorization runs before the body is required, and the write is
/// `change_experiment`, the same function the CLI tool calls.
pub(in crate::api) async fn diagnostics_experiment_conclude(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Result<Json<ConcludeBody>, JsonRejection>,
) -> Response {
    if let Err(denial) = super::super::routines::authorized_caller(
        &DASHBOARD_EXPERIMENT_CONCLUDE,
        state.operator_session(),
    ) {
        return super::super::routines::authorization_denied(denial);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let reason = body.reason.trim().to_string();
    if reason.is_empty() {
        return super::super::bad_request(
            "conclude needs a reason; it is recorded with the decision".into(),
        );
    }
    let arm = body
        .arm
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match super::super::blocking("experiment conclude", move || {
        runtime.change_experiment(
            &id,
            ExperimentChange::Conclude { arm },
            ExperimentActor {
                actor: "dashboard".into(),
                reason: Some(reason),
            },
        )
    })
    .await
    {
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
