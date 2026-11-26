//! Task approval, rejection, recovery, settlement and archival.

use super::DASHBOARD_ACTOR;
use super::request::{ApproveBody, ArchiveBody, RejectBody, RequeueBody};
use super::writes::{self, WebTaskMutationMeta, run_audited_web_task_mutation};
use crate::api::helpers;
use crate::api::routines::{authorization_denied, authorized_caller};
use crate::api::{OptionalJson, bad_request, blocking, map_runtime_error, validate_id};
use crate::state::{DashboardState, WsCore};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_TASK_REQUEUE, DASHBOARD_TASK_SETTLE_MERGED,
};
use orbit_core::app::task::{DashboardAction, Outcome};
use orbit_core::application::task::ExternalMergeSettlementProfile;
use serde_json::json;

pub(in crate::api) async fn approve_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<ApproveBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let meta = WebTaskMutationMeta::new(&state, &headers, "approve", Some(id.clone()), json!({}));
    writes::task_mutation_response(
        core,
        "task approval",
        meta,
        DashboardAction::Approve {
            id,
            note: body.note,
            comment: body.comment,
        },
    )
    .await
}

pub(in crate::api) async fn reject_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
    Path(id): Path<String>,
    Json(body): Json<RejectBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let id = id.to_string();
    let meta = WebTaskMutationMeta::new(&state, &headers, "reject", Some(id.clone()), json!({}));
    writes::task_mutation_response(
        core,
        "task rejection",
        meta,
        DashboardAction::Reject {
            id,
            note: body.note,
            comment: body.comment,
        },
    )
    .await
}

/// Map a trusted human-write error onto the operator routes' refusal contract.
/// A missing task is 404 `task_not_found`; invalid input is 409 with `code`.
pub(super) fn recovery_refusal(error: orbit_core::OrbitError, code: &'static str) -> Response {
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

/// `POST /tasks/:id/requeue` — operator-gated blocked requeue, or discard a
/// stopped candidate from any open task.
pub(in crate::api) async fn requeue_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
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
    let mut meta = WebTaskMutationMeta::new(
        &state,
        &headers,
        "requeue",
        Some(id.clone()),
        json!({ "discard_candidate": discard }),
    );
    meta.role = Some(DASHBOARD_ACTOR.to_string());
    let result = helpers::blocking_join("task requeue", move || {
        run_audited_web_task_mutation(
            &core,
            meta,
            DashboardAction::Requeue {
                id,
                reason,
                discard_candidate: discard,
                actor: DASHBOARD_ACTOR,
            },
        )
        .and_then(Outcome::into_answer)
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => recovery_refusal(e, "requeue_refused"),
        Err(response) => *response,
    }
}

/// `POST /tasks/:id/settle-merged` — operator-gated settle from a merged PR.
pub(in crate::api) async fn settle_merged_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
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
    let mut meta = WebTaskMutationMeta::new(
        &state,
        &headers,
        "settle_merged",
        Some(id.clone()),
        json!({}),
    );
    meta.role = Some(DASHBOARD_ACTOR.to_string());
    let result = helpers::blocking_join("task settle-merged", move || {
        run_audited_web_task_mutation(
            &core,
            meta,
            DashboardAction::SettleMerged {
                id,
                actor: DASHBOARD_ACTOR,
            },
        )
        .and_then(|outcome| {
            outcome.into_settled()?.pop().ok_or_else(|| {
                orbit_core::OrbitError::Execution(
                    "settle-merged answered no settlement".to_string(),
                )
            })
        })
    })
    .await;
    match result {
        Ok(Ok(settlement)) => {
            Json(settlement.settlement_json(ExternalMergeSettlementProfile::Http)).into_response()
        }
        Ok(Err(e)) => recovery_refusal(e, "settle_merged_refused"),
        Err(response) => *response,
    }
}

pub(in crate::api) async fn archive_task_action(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    WsCore(core): WsCore,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<ArchiveBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id,
        Err(message) => return bad_request(message),
    };
    let archived_id = id.to_string();
    let force = body.force;
    let meta = WebTaskMutationMeta::new(
        &state,
        &headers,
        "archive",
        Some(archived_id.clone()),
        json!({ "force": force }),
    );
    match blocking("task archival", move || {
        run_audited_web_task_mutation(
            &core,
            meta,
            DashboardAction::Archive {
                id: archived_id,
                force,
            },
        )
    })
    .await
    {
        Ok(_) => Json(json!({ "ok": true, "id": id })).into_response(),
        Err(response) => *response,
    }
}
