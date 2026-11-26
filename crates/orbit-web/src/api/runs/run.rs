//! Single-run detail, cancel, and replay endpoints.

use crate::state::DashboardState;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_core::application::job::JobRunCancelResultProfile;
use serde_json::json;

use super::super::routines::{OperationsQuery, authorization_denied, explicit_workspace};
use super::super::{OptionalJson, bad_request, blocking, map_runtime_error, validate_id};
use super::locate::{RunLookupFailure, requested_workspace, with_run};
use orbit_core::app::run::{Command, DashboardReplay, exec};

/// `GET /api/runs/:id`. The run is looked up in the requested workspace and
/// then under every workspace id this host's store records it, so a link
/// naming the wrong workspace, or a run recorded under a workspace since
/// removed from the host, still opens. See [`super::locate`].
pub(in crate::api) async fn get_run(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
    Path(id): Path<String>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let requested = requested_workspace(query.as_deref());
    match blocking("get run", move || {
        Ok(with_run(
            &state,
            requested.as_deref(),
            &id,
            |core, lookup| exec(core, Command::dashboard_detail(lookup)),
        ))
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(RunLookupFailure::Response(response))) => *response,
        Ok(Err(RunLookupFailure::Error(error))) => map_runtime_error(*error),
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

/// `POST /api/runs/:id/cancel`. Acts on the registered runtime that holds the
/// run, which need not be the requested workspace: see [`with_run`].
pub(in crate::api) async fn cancel_run_action(
    State(state): State<DashboardState>,
    RawQuery(query): RawQuery,
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
    let requested = requested_workspace(query.as_deref());
    match blocking("cancel run", move || {
        Ok(with_run(
            &state,
            requested.as_deref(),
            &id,
            |core, lookup| {
                exec(
                    core,
                    Command::dashboard_cancel(
                        lookup,
                        body.queued_only,
                        body.reason.as_deref(),
                        body.force,
                        body.block,
                    ),
                )
            },
        ))
    })
    .await
    {
        Ok(Ok(Ok(result))) => Json(result.to_json(JobRunCancelResultProfile::Http)).into_response(),
        Ok(Ok(Err(orbit_core::OrbitError::JobValidation(msg))))
        | Ok(Ok(Err(orbit_core::OrbitError::JobRunStateTransition(msg)))) => {
            (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response()
        }
        Ok(Ok(Err(e))) => map_runtime_error(e),
        Ok(Err(failure)) => failure.into_response(),
        Err(response) => *response,
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct ReplayBody {
    #[serde(default)]
    claim_token: Option<String>,
}

/// `POST /api/runs/:id/replay`. The replay is submitted to the registered
/// runtime that holds the source run, which need not be the requested
/// workspace: see [`with_run`].
pub(in crate::api) async fn replay_run_action(
    State(state): State<DashboardState>,
    RawQuery(raw_query): RawQuery,
    Path(id): Path<String>,
    Query(query): Query<OperationsQuery>,
    OptionalJson(body): OptionalJson<ReplayBody>,
) -> Response {
    if let Err(response) = explicit_workspace(&query) {
        return response.into_response();
    }
    let replay = match DashboardReplay::admit(state.operator_session()) {
        Ok(replay) => replay,
        Err(denial) => return authorization_denied(denial),
    };
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let requested = requested_workspace(raw_query.as_deref());
    match blocking("replay run", move || {
        Ok(with_run(
            &state,
            requested.as_deref(),
            &id,
            |core, lookup| {
                exec(
                    core,
                    Command::dashboard_replay(lookup, body.claim_token.as_deref(), replay),
                )
            },
        ))
    })
    .await
    {
        Ok(Ok(Ok(result))) => Json(json!({
            "run_id": result.run_id,
            "state": if result.queued { "queued" } else { "submitted" },
        }))
        .into_response(),
        Ok(Ok(Err(denial))) => authorization_denied(denial),
        Ok(Err(failure)) => failure.into_response(),
        Err(response) => *response,
    }
}
