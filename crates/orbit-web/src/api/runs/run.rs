//! Single-run detail, cancel, and replay endpoints.

use crate::state::{DashboardState, Ws};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_JOB_RUN};
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
    match blocking("cancel run", move || {
        Ok(runtime.cancel_job_run_with_options_and_policy(
            &id,
            "dashboard",
            "web",
            body.reason.as_deref(),
            body.force,
            body.block,
        ))
    })
    .await
    {
        Ok(Ok(result)) => Json(json!({
            "run_id": result.run_id,
            "outcome": result.outcome,
            "previous_state": result.previous_state,
            "final_state": result.final_state,
            "actor": result.actor,
            "source": result.source,
            "signal_attempted": result.signal_attempted,
            "signal_outcome": result.signal_outcome,
            "provider_processes_stopped": result.provider_processes_stopped,
            // [ORB-13663] Cancelling a follower pull drain settles what it
            // carried: recorded settlements are delivered and claims nothing
            // will launch go back to the owner's backlog.
            "pull_settlements": result.pull_settlements,
            // A gracefully cancelled pull drain is `cancelling` until these
            // leaves finish; a forced cancel names what it stopped, and what
            // it could not confirm stopped (those keep their claims).
            "waiting_leaves": result.waiting_leaves,
            "forced_runs": result.forced_runs,
            "unstopped_leaves": result.unstopped_leaves,
            "unstopped_children": result.unstopped_children,
        }))
        .into_response(),
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
