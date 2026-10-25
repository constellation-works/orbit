//! `/tasks/:id/auto-fix`: the details panel's "Fix PR" action.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_DRAIN_AUTO_FIX, DASHBOARD_AUTO_DRAIN_COMPLETE,
};
use orbit_core::application::job::TaskAutoFixError;
use serde_json::json;

use super::routines::{authorization_denied, authorized_caller};
use super::{bad_request, blocking, map_runtime_error, server_error, validate_id};
use crate::state::{DashboardState, Ws};

/// Whether Fix PR would start a repair of this task's pull request now
/// (`GET /tasks/:id/auto-fix`), so the panel can enable the button or name
/// why it is disabled. Reads the forge once; starts nothing.
///
/// `authorized` reports the operator session's repair authority; without it
/// the forge is not read and `reason` is `not_authorized`.
pub(super) async fn task_auto_fix_readiness(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let operator_session = state.operator_session();
    if let Err(denial) = authorized_caller(&DASHBOARD_AUTO_DRAIN_AUTO_FIX, operator_session) {
        return Json(json!({
            "authorized": false,
            "available": false,
            "reason": "not_authorized",
            "detail": denial.to_string(),
            "pr_number": null,
            "blocker": null,
            "head_sha": null,
        }))
        .into_response();
    }
    let completion_done =
        authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, operator_session).is_ok();
    match blocking("task auto-fix readiness", move || {
        runtime.task_pr_auto_fix_readiness(&id, completion_done)
    })
    .await
    {
        Ok(mut readiness) => {
            readiness["authorized"] = json!(true);
            Json(readiness).into_response()
        }
        Err(response) => *response,
    }
}

/// Start an out-of-run auto-fix of this task's pull request
/// (`POST /tasks/:id/auto-fix`): the repair an `orbit run auto --auto-fix`
/// drain would start for it, through the same runtime path, with no drain.
///
/// Requires the operator session's auto-fix authority, as `auto_fix` on a
/// dashboard drain does. The resumed delivery keeps its own completion
/// setting; one started with `--complete` is resumed only when the session
/// also holds completion authority. Every refusal is a `409` whose `code` is
/// `task_auto_fix_<reason>`; nothing is started.
pub(super) async fn start_task_auto_fix(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let operator_session = state.operator_session();
    if let Err(denial) = authorized_caller(&DASHBOARD_AUTO_DRAIN_AUTO_FIX, operator_session) {
        return authorization_denied(denial);
    }
    let completion_done =
        authorized_caller(&DASHBOARD_AUTO_DRAIN_COMPLETE, operator_session).is_ok();
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let started = tokio::task::spawn_blocking(move || {
        runtime.start_task_pr_auto_fix(&id, completion_done, Some("dashboard"))
    })
    .await;
    match started {
        Ok(Ok(started)) => Json(started).into_response(),
        Ok(Err(TaskAutoFixError::Refused(refusal))) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": refusal.to_string(),
                "code": refusal.code(),
                "reason": refusal.reason(),
            })),
        )
            .into_response(),
        Ok(Err(TaskAutoFixError::Runtime(error))) => map_runtime_error(error),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task auto-fix start panicked: {join_err}"
        ))),
    }
}
