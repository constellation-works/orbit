//! Task approval, rejection, recovery, settlement and archival.

use super::DASHBOARD_ACTOR;
use super::request::{ApproveBody, RejectBody, RequeueBody};
use super::writes;
use crate::api::routines::{authorization_denied, authorized_caller};
use crate::api::{
    OptionalJson, bad_request, blocking, map_runtime_error, server_error, validate_id,
};
use crate::state::{DashboardState, Ws};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_TASK_REQUEUE, DASHBOARD_TASK_SETTLE_MERGED,
};
use orbit_core::application::task::{ExternalMergeSettlementProfile, ScreenedTaskWrite};
use serde_json::json;

pub(in crate::api) async fn approve_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<ApproveBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    writes::task_mutation_response(runtime, "task approval", move |runtime| {
        runtime.approve_task_screened(&id, body.note, body.comment, None, None)
    })
    .await
}

pub(in crate::api) async fn reject_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<RejectBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    writes::task_mutation_response(runtime, "task rejection", move |runtime| {
        runtime
            .reject_task(&id, body.note, body.comment)
            .map(ScreenedTaskWrite::from)
    })
    .await
}

fn recovery_refusal(error: orbit_core::OrbitError, code: &'static str) -> Response {
    match error {
        orbit_core::OrbitError::NotFound {
            kind: orbit_core::NotFoundKind::Task,
            id,
            guidance,
        } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("task not found: {id}{}", guidance.as_deref().unwrap_or_default()), "code": "task_not_found" })),
        )
            .into_response(),
        orbit_core::OrbitError::InvalidInput(message) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": message, "code": code })),
        )
            .into_response(),
        other => map_runtime_error(other),
    }
}

/// `POST /tasks/:id/requeue` — operator-gated requeue of a blocked task.
pub(in crate::api) async fn requeue_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<RequeueBody>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_TASK_REQUEUE, state.operator_session()) {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let reason = body.reason.unwrap_or_default();
    if reason.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "requeue needs a reason; it is recorded with the decision",
                "code": "requeue_reason_required"
            })),
        )
            .into_response();
    }
    let discard = body.discard_candidate;
    let result = tokio::task::spawn_blocking(move || {
        runtime.requeue_blocked_task_as(&id, &reason, discard, Some(DASHBOARD_ACTOR))
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => recovery_refusal(e, "requeue_refused"),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task requeue panicked: {join_err}"
        ))),
    }
}

/// `POST /tasks/:id/settle-merged` — operator-gated settle from a merged PR.
pub(in crate::api) async fn settle_merged_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_TASK_SETTLE_MERGED, state.operator_session())
    {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let result = tokio::task::spawn_blocking(move || {
        runtime.settle_externally_merged_task_as(&id, Some(DASHBOARD_ACTOR))
    })
    .await;
    match result {
        Ok(Ok(settlement)) => {
            Json(settlement.settlement_json(ExternalMergeSettlementProfile::Http)).into_response()
        }
        Ok(Err(e)) => recovery_refusal(e, "settle_merged_refused"),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task settle-merged panicked: {join_err}"
        ))),
    }
}

/// `POST /tasks/settle-merged` — operator-gated batch settle from merged PRs:
/// every blocked, in-progress and review task in the selected workspace whose
/// own recorded delivery PR merged settles; the rest are reported skipped.
pub(in crate::api) async fn settle_merged_tasks_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_TASK_SETTLE_MERGED, state.operator_session())
    {
        return authorization_denied(denial);
    }
    let result = tokio::task::spawn_blocking(move || {
        runtime.settle_externally_merged_tasks_from_recorded_prs_as(Some(DASHBOARD_ACTOR))
    })
    .await;
    match result {
        Ok(Ok(batch)) => Json(json!({
            "settled": batch.settled.iter().map(|settlement|
                settlement.settlement_json(ExternalMergeSettlementProfile::Http)
            ).collect::<Vec<_>>(),
            "skipped": batch.skipped.iter().map(|skipped| json!({
                "task_id": skipped.task_id,
                "code": "settle_merged_refused",
                "reason": skipped.reason,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Ok(Err(e)) => map_runtime_error(e),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task settle-merged batch panicked: {join_err}"
        ))),
    }
}

pub(in crate::api) async fn archive_task_action(
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let archived_id = id.to_string();
    let runtime_clone = runtime.clone();
    match blocking("task archival", move || {
        runtime_clone.archive_task(&archived_id)
    })
    .await
    {
        Ok(()) => Json(json!({ "ok": true, "id": id })).into_response(),
        Err(response) => *response,
    }
}
