//! Operator pilot admission and post-deploy records.

use super::DASHBOARD_ACTOR;
use super::request::{PilotAdmissionBody, PostDeployBody};
use crate::api::routines::{authorization_denied, authorized_caller};
use crate::api::{
    OptionalJson, bad_request, map_runtime_error, non_empty_string, server_error, validate_id,
};
use crate::state::{DashboardState, Ws};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_TASK_PILOT_ADMISSION, DASHBOARD_TASK_POST_DEPLOY,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{
    PostDeployOperatorRecord, PostDeployRecordRefusal, TaskUpdateParams,
};
use orbit_types::workflow::PostDeployOutcome;
use serde_json::json;
use std::sync::Arc;

fn operator_record_refusal(error: orbit_core::OrbitError, code: &'static str) -> Response {
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

fn evidence_required(code: &'static str, message: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message, "code": code })),
    )
        .into_response()
}

/// `POST /tasks/:id/pilot-admission` — operator-gated record of a
/// task-pilot admission decision (`clear` or `approve-anyway`) with evidence.
/// Writes the exact human comment grammar core parses, as a human operator
/// write, so the hold resolves exactly as the CLI comment does.
pub(in crate::api) async fn pilot_admission_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<PilotAdmissionBody>,
) -> Response {
    if let Err(denial) =
        authorized_caller(&DASHBOARD_TASK_PILOT_ADMISSION, state.operator_session())
    {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let raw_verdict = body
        .verdict
        .or(body.decision)
        .or(body.action)
        .unwrap_or_default();
    let verdict = match raw_verdict
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-")
        .as_str()
    {
        "clear" => "clear",
        "approve-anyway" => "approve-anyway",
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "pilot admission needs a verdict of `clear` or `approve-anyway`",
                    "code": "pilot_admission_verdict_invalid"
                })),
            )
                .into_response();
        }
    };
    let Some(evidence) = body
        .evidence
        .or(body.reason)
        .as_deref()
        .and_then(non_empty_string)
    else {
        return evidence_required(
            "pilot_admission_evidence_required",
            "pilot admission needs evidence; it is recorded with the decision",
        );
    };
    let comment = format!("task-pilot-admission: {verdict}\n{evidence}");
    let task_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        if !runtime.has_resolvable_pilot_admission_hold(&task_id)? {
            return Err(orbit_core::OrbitError::InvalidInput(format!(
                "task {task_id} has no pilot admission hold to record a decision for"
            )));
        }
        let params = TaskUpdateParams {
            comment: Some(comment),
            ..TaskUpdateParams::default()
        };
        runtime.update_task_as_human(&task_id, params, DASHBOARD_ACTOR.to_string())
    })
    .await;
    match result {
        Ok(Ok(task)) => Json(json!({
            "id": task.id,
            "status": task.status,
            "verdict": verdict,
        }))
        .into_response(),
        Ok(Err(e)) => operator_record_refusal(e, "pilot_admission_no_matching_hold"),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task pilot admission panicked: {join_err}"
        ))),
    }
}

/// Shared implementation for the two post-deploy route shapes: core
/// normalizes the record and checks the owed criterion.
async fn record_post_deploy_met(
    state: DashboardState,
    runtime: Arc<OrbitRuntime>,
    id: String,
    criterion: Option<usize>,
    evidence: Option<String>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_TASK_POST_DEPLOY, state.operator_session()) {
        return authorization_denied(denial);
    }
    let record = match PostDeployOperatorRecord::from_operator_input(
        criterion,
        PostDeployOutcome::Met,
        evidence.as_deref(),
    ) {
        Ok(record) => record,
        Err(PostDeployRecordRefusal::CriterionRequired) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "post-deploy record needs a 1-based criterion index",
                    "code": "post_deploy_criterion_required"
                })),
            )
                .into_response();
        }
        Err(PostDeployRecordRefusal::EvidenceRequired) => {
            return evidence_required(
                "post_deploy_evidence_required",
                "post-deploy record needs evidence; it is recorded with the measurement",
            );
        }
    };
    let criterion = record.criterion();
    let result = tokio::task::spawn_blocking(move || {
        runtime.record_post_deploy_as_human(&id, &record, DASHBOARD_ACTOR.to_string())
    })
    .await;
    match result {
        Ok(Ok(task)) => Json(json!({
            "id": task.id,
            "status": task.status,
            "criterion": criterion,
            "outcome": "met",
        }))
        .into_response(),
        Ok(Err(e)) => operator_record_refusal(e, "post_deploy_criterion_not_owed"),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task post-deploy record panicked: {join_err}"
        ))),
    }
}

/// `POST /tasks/:id/post-deploy` — operator-gated `post-deploy: met <N>`
/// record with evidence, in the body.
pub(in crate::api) async fn post_deploy_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<PostDeployBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    record_post_deploy_met(
        state,
        runtime,
        id,
        body.criterion,
        body.evidence.or(body.reason),
    )
    .await
}

/// `POST /tasks/:id/post-deploy/:criterion/met` — same record, criterion in
/// the path.
pub(in crate::api) async fn post_deploy_met_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path((id, criterion)): Path<(String, String)>,
    OptionalJson(body): OptionalJson<PostDeployBody>,
) -> Response {
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    // Parsed here rather than by the extractor, so the operator check runs
    // before a malformed index is refused.
    record_post_deploy_met(
        state,
        runtime,
        id,
        criterion.parse().ok(),
        body.evidence.or(body.reason),
    )
    .await
}
