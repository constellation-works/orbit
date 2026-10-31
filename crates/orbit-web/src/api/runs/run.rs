//! Single-run detail, cancel, and replay endpoints.

use crate::state::{DashboardState, Ws};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_JOB_RUN};
use orbit_core::application::job::JobRunCancelResultProfile;
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

use super::super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
};
use super::super::{OptionalJson, bad_request, blocking, map_runtime_error, validate_id};
use super::detail::job_run_detail_to_json;

pub(in crate::api) async fn get_run(Ws(runtime): Ws, Path(id): Path<String>) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    match blocking("get run", move || {
        let run = runtime.show_job_run(&id)?;
        Ok(job_run_detail_to_json(&runtime, &run))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
pub(in crate::api) struct CancelRunBody {
    #[serde(default)]
    reason: Option<String>,
    /// Stop a drain's in-flight leaves instead of waiting for them.
    #[serde(default)]
    force: bool,
    /// Keep a cancelled task leaf blocked instead of returning it to backlog.
    #[serde(default)]
    block: bool,
    /// Cancel only while the run is still queued; one that has started is
    /// refused with 409 and left running.
    #[serde(default)]
    queued_only: bool,
}

pub(in crate::api) async fn cancel_run_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<CancelRunBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    if body.queued_only && body.force {
        return bad_request("queued_only cannot be combined with force".to_string());
    }
    match blocking("cancel run", move || {
        Ok(if body.queued_only {
            runtime.cancel_queued_job_run(
                &id,
                "dashboard",
                "web",
                body.reason.as_deref(),
                body.block,
            )
        } else {
            runtime.cancel_job_run_with_options_and_policy(
                &id,
                "dashboard",
                "web",
                body.reason.as_deref(),
                body.force,
                body.block,
            )
        })
    })
    .await
    {
        Ok(Ok(result)) => Json(result.to_json(JobRunCancelResultProfile::Http)).into_response(),
        Ok(Err(orbit_core::OrbitError::JobValidation(msg)))
        | Ok(Err(orbit_core::OrbitError::JobRunStateTransition(msg))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Err(e)) => map_runtime_error(e),
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ReplayBody {
    #[serde(default)]
    claim_token: Option<String>,
}

pub(in crate::api) async fn replay_run_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Query(query): Query<OperationsQuery>,
    OptionalJson(body): OptionalJson<ReplayBody>,
) -> Response {
    if let Err(response) = explicit_workspace(&query) {
        return response.into_response();
    }
    if let Err(denial) = authorized_caller(&DASHBOARD_JOB_RUN, state.operator_session()) {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let completion_authority =
        authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, state.operator_session());
    match blocking("replay run", move || {
        let source = runtime.show_job_run(&id)?;
        if source
            .input
            .as_ref()
            .and_then(|input| input.get("completion"))
            .and_then(Value::as_str)
            == Some("done")
            && let Err(denial) = completion_authority
        {
            return Ok(Err(denial));
        }
        runtime
            .submit_replay_run(
                &id,
                Some("dashboard"),
                body.claim_token.as_deref(),
                JobRunTrigger::dashboard(),
            )
            .map(Ok)
    })
    .await
    {
        Ok(Ok(result)) => Json(json!({
            "run_id": result.run_id,
            "state": if result.queued { "queued" } else { "submitted" },
        }))
        .into_response(),
        Ok(Err(denial)) => authorization_denied(denial),
        Err(response) => *response,
    }
}
