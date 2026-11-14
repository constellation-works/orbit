//! Operator authoring on Analytics › Experiments: `POST /diagnostics/experiments`
//! defines a draft, `PATCH /diagnostics/experiments/{id}` edits one and
//! `POST /diagnostics/experiments/{id}/start` starts it, and `.../pause` and
//! `.../resume` pause and resume it. Each authorizes its dashboard operation
//! before reading the body, then calls the same core lifecycle function as
//! `orbit.experiment.create`, `update`, `start`, `pause` and `resume`, so
//! core's validation, refusals and the event it records (actor `dashboard`,
//! with the submitted reason) are the CLI's.

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_EXPERIMENT_CREATE, DASHBOARD_EXPERIMENT_PAUSE, DASHBOARD_EXPERIMENT_RESUME,
    DASHBOARD_EXPERIMENT_START, DASHBOARD_EXPERIMENT_UPDATE, GovernedOperation,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    EXPERIMENT_COMPLEXITIES, ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
    ExperimentView, experiment_metric_names, step_local_metric_names,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::api::OptionalJson;
use crate::state::{DashboardState, Ws};

/// The actor every dashboard experiment event records, as conclude does.
const DASHBOARD_ACTOR: &str = "dashboard";

/// What the New experiment form and the draft actions need: whether this
/// session may create, edit and start, the workspace's `[experiments]`
/// settings, the metric and complexity names core accepts and the configured
/// crews a crew pool may name.
pub(super) fn experiment_authoring(runtime: &OrbitRuntime, operator_session: bool) -> Value {
    let capability = |operation: &'static GovernedOperation| {
        crate::api::routines::action_capability(operation, operator_session)
    };
    let settings = runtime.experiment_settings();
    let crews = runtime
        .configured_crew_registry_projection()
        .crews
        .into_iter()
        .map(|crew| json!({ "name": crew.name, "enabled": crew.enabled }))
        .collect::<Vec<_>>();
    json!({
        "create": capability(&DASHBOARD_EXPERIMENT_CREATE),
        "update": capability(&DASHBOARD_EXPERIMENT_UPDATE),
        "start": capability(&DASHBOARD_EXPERIMENT_START),
        "pause": capability(&DASHBOARD_EXPERIMENT_PAUSE),
        "resume": capability(&DASHBOARD_EXPERIMENT_RESUME),
        "enabled": settings.enabled,
        "max_running": orbit_core::application::experiment::EXPERIMENTS_MAX_RUNNING,
        "config_keys": settings.config_keys,
        "metrics": experiment_metric_names().collect::<Vec<_>>(),
        "step_local_metrics": step_local_metric_names().collect::<Vec<_>>(),
        "complexities": EXPERIMENT_COMPLEXITIES,
        "crews": crews,
    })
}

/// A create or edit body: the fields `orbit.experiment.create` and `update`
/// take, plus the reason the event records. An absent field is left as it is.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ExperimentDefinitionBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    hypothesis: Option<String>,
    #[serde(default)]
    arms: Option<Value>,
    #[serde(default)]
    population: Option<Value>,
    #[serde(default)]
    primary_metric: Option<String>,
    #[serde(default)]
    guardrails: Option<Value>,
    #[serde(default)]
    min_sample: Option<usize>,
    #[serde(default)]
    max_days: Option<usize>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

impl ExperimentDefinitionBody {
    /// The core input and the actor; `name` is the rename an edit carries.
    fn into_input(self, name: Option<String>) -> (ExperimentDefinitionInput, ExperimentActor) {
        let input = ExperimentDefinitionInput {
            name,
            hypothesis: self.hypothesis,
            arms: self.arms,
            population: self.population,
            primary_metric: self.primary_metric,
            guardrails: self.guardrails,
            min_sample: self.min_sample,
            max_days: self.max_days,
            mode: self.mode,
        };
        (input, dashboard_actor(self.reason))
    }
}

/// The optional body of a lifecycle change (start, pause, resume): the reason
/// the event records. An empty body is a change with no reason.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ExperimentReasonBody {
    #[serde(default)]
    reason: Option<String>,
}

fn dashboard_actor(reason: Option<String>) -> ExperimentActor {
    ExperimentActor {
        actor: DASHBOARD_ACTOR.into(),
        reason: reason
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    }
}

/// `POST /diagnostics/experiments` — define a draft through
/// `create_experiment`.
pub(in crate::api) async fn diagnostics_experiment_create(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    body: Result<Json<ExperimentDefinitionBody>, JsonRejection>,
) -> Response {
    if let Some(denied) = refuse_unauthorized(&DASHBOARD_EXPERIMENT_CREATE, &state) {
        return denied;
    }
    let Json(mut body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    // Core validates the name, so a missing one is its refusal, not ours.
    let name = body.name.take().unwrap_or_default();
    let (input, by) = body.into_input(None);
    experiment_write(runtime, "experiment create", move |runtime| {
        runtime.create_experiment(&name, input, by)
    })
    .await
}

/// `PATCH /diagnostics/experiments/{id}` — edit a draft through
/// `update_experiment`, which refuses a started experiment.
pub(in crate::api) async fn diagnostics_experiment_update(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Result<Json<ExperimentDefinitionBody>, JsonRejection>,
) -> Response {
    if let Some(denied) = refuse_unauthorized(&DASHBOARD_EXPERIMENT_UPDATE, &state) {
        return denied;
    }
    let Json(mut body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let name = body.name.take();
    let (input, by) = body.into_input(name);
    experiment_write(runtime, "experiment update", move |runtime| {
        runtime.update_experiment(&id, input, by)
    })
    .await
}

/// `POST /diagnostics/experiments/{id}/start` — start a draft through
/// `change_experiment`.
pub(in crate::api) async fn diagnostics_experiment_start(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Result<OptionalJson<ExperimentReasonBody>, Response>,
) -> Response {
    experiment_lifecycle(
        &state,
        runtime,
        id,
        body,
        &DASHBOARD_EXPERIMENT_START,
        "experiment start",
        ExperimentChange::Start,
    )
    .await
}

/// `POST /diagnostics/experiments/{id}/pause` — pause a running experiment
/// through `change_experiment`, as `orbit experiment pause` does.
pub(in crate::api) async fn diagnostics_experiment_pause(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Result<OptionalJson<ExperimentReasonBody>, Response>,
) -> Response {
    experiment_lifecycle(
        &state,
        runtime,
        id,
        body,
        &DASHBOARD_EXPERIMENT_PAUSE,
        "experiment pause",
        ExperimentChange::Pause,
    )
    .await
}

/// `POST /diagnostics/experiments/{id}/resume` — resume a paused experiment
/// through `change_experiment`, as `orbit experiment resume` does.
pub(in crate::api) async fn diagnostics_experiment_resume(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    body: Result<OptionalJson<ExperimentReasonBody>, Response>,
) -> Response {
    experiment_lifecycle(
        &state,
        runtime,
        id,
        body,
        &DASHBOARD_EXPERIMENT_RESUME,
        "experiment resume",
        ExperimentChange::Resume,
    )
    .await
}

/// One lifecycle change: authorize the operation before the body is read,
/// then run `change_experiment` with the dashboard actor and the body's
/// reason. The body may be empty.
async fn experiment_lifecycle(
    state: &DashboardState,
    runtime: Arc<OrbitRuntime>,
    id: String,
    body: Result<OptionalJson<ExperimentReasonBody>, Response>,
    operation: &'static GovernedOperation,
    label: &'static str,
    change: ExperimentChange,
) -> Response {
    if let Some(denied) = refuse_unauthorized(operation, state) {
        return denied;
    }
    let OptionalJson(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    let by = dashboard_actor(body.reason);
    experiment_write(runtime, label, move |runtime| {
        runtime.change_experiment(&id, change, by)
    })
    .await
}

fn refuse_unauthorized(
    operation: &'static GovernedOperation,
    state: &DashboardState,
) -> Option<Response> {
    crate::api::routines::authorized_caller(operation, state.operator_session())
        .err()
        .map(crate::api::routines::authorization_denied)
}

/// Run one core write and answer its view. A workspace policy refusal
/// (`experiments.enabled` false, three experiments already running) is a 409
/// carrying core's message; every other refusal maps as elsewhere, keeping
/// core's text.
async fn experiment_write<F>(runtime: Arc<OrbitRuntime>, label: &'static str, write: F) -> Response
where
    F: FnOnce(&OrbitRuntime) -> Result<ExperimentView, orbit_core::OrbitError> + Send + 'static,
{
    match crate::api::blocking(label, move || Ok(write(&runtime))).await {
        Ok(Ok(view)) => Json(view).into_response(),
        Ok(Err(orbit_core::OrbitError::PolicyDenied(message))) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": message, "code": "experiment_policy_denied" })),
        )
            .into_response(),
        Ok(Err(error)) => crate::api::map_runtime_error(error),
        Err(response) => *response,
    }
}
