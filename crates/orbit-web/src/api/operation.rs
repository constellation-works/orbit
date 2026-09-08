//! Operation-mode explanation and grant controls for the dashboard
//! [ORB-11332].
//!
//! The explanation is the same projection `orbit operation explain` prints.
//! Stop and revoke are governed operator actions: a caller without the
//! operator capability is refused before any runtime call, and every
//! decision is recorded through the dashboard operations audit.

use std::time::Instant;

use crate::state::Ws;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_OPERATION_REVOKE, DASHBOARD_OPERATION_STOP, GovernedOperation,
};
use orbit_core::{OperationGrantControlRequest, OperationGrantControlResult, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use super::map_runtime_error;
use super::routines::{authorization_denied, authorized_caller, record_operation_audit};

#[derive(Deserialize, Default)]
pub(super) struct GrantControlBody {
    /// The grant to change; omitted selects the workspace's active grant.
    #[serde(default)]
    grant_id: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    /// Compare-and-set expectation from the rendered explanation.
    #[serde(default)]
    expected_revision: Option<u32>,
    #[serde(default)]
    claim_token: Option<String>,
}

/// `GET /operation/explain?workspace=<id>` — the effective policy, authority,
/// caps, and limiting reasons, plus whether this caller may stop or revoke.
pub(super) async fn explain_operation(Ws(runtime): Ws) -> Response {
    match runtime.explain_operation(None) {
        Ok(mut payload) => {
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "controls_authorized".to_string(),
                    Value::Bool(authorized_caller(&DASHBOARD_OPERATION_STOP).is_ok()),
                );
            }
            Json(payload).into_response()
        }
        Err(error) => map_runtime_error(error),
    }
}

/// `POST /operation/stop?workspace=<id>` — stop new admissions under a grant.
pub(super) async fn stop_operation_action(
    Ws(runtime): Ws,
    body: Option<Json<GrantControlBody>>,
) -> Response {
    let Json(body) = body.unwrap_or_default();
    let caller =
        match authorize_control(&runtime, &DASHBOARD_OPERATION_STOP, "operation.stop", &body) {
            Ok(caller) => caller,
            Err(response) => return *response,
        };
    let grant_id = body.grant_id.clone();
    let reason = body.reason.clone();
    let expected_revision = body.expected_revision;
    let claim_token = body.claim_token.clone();
    let started = Instant::now();
    let result = match blocking("operation stop", {
        let runtime = runtime.clone();
        move || {
            Ok(runtime.stop_operation_grant(OperationGrantControlRequest {
                grant_id: grant_id.as_deref(),
                reason: reason.as_deref(),
                expected_revision,
                actor: "dashboard",
                source: "dashboard",
                claim_token: claim_token.as_deref(),
            }))
        }
    })
    .await
    {
        Ok(inner) => inner,
        Err(response) => return *response,
    };
    finish_control(&runtime, "operation.stop", &body, &caller, started, result)
}

/// `POST /operation/revoke?workspace=<id>` — hard-revoke a grant.
pub(super) async fn revoke_operation_action(
    Ws(runtime): Ws,
    body: Option<Json<GrantControlBody>>,
) -> Response {
    let Json(body) = body.unwrap_or_default();
    let caller = match authorize_control(
        &runtime,
        &DASHBOARD_OPERATION_REVOKE,
        "operation.revoke",
        &body,
    ) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let grant_id = body.grant_id.clone();
    let reason = body.reason.clone();
    let expected_revision = body.expected_revision;
    let claim_token = body.claim_token.clone();
    let started = Instant::now();
    let result = match blocking("operation revoke", {
        let runtime = runtime.clone();
        move || {
            Ok(
                runtime.revoke_operation_grant(OperationGrantControlRequest {
                    grant_id: grant_id.as_deref(),
                    reason: reason.as_deref(),
                    expected_revision,
                    actor: "dashboard",
                    source: "dashboard",
                    claim_token: claim_token.as_deref(),
                }),
            )
        }
    })
    .await
    {
        Ok(inner) => inner,
        Err(response) => return *response,
    };
    finish_control(
        &runtime,
        "operation.revoke",
        &body,
        &caller,
        started,
        result,
    )
}

fn authorize_control(
    runtime: &OrbitRuntime,
    governed: &'static GovernedOperation,
    operation: &str,
    body: &GrantControlBody,
) -> Result<orbit_common::governance::authorization::CallerCapabilities, Box<Response>> {
    let workspace = runtime.workspace_id().unwrap_or_default();
    let target = body
        .grant_id
        .clone()
        .unwrap_or_else(|| "active".to_string());
    let arguments = json!({
        "grant_id": body.grant_id,
        "reason": body.reason,
        "expected_revision": body.expected_revision,
    });
    match authorized_caller(governed) {
        Ok(caller) => Ok(caller),
        Err(denial) => {
            record_operation_audit(
                runtime,
                &workspace,
                operation,
                &target,
                "",
                &arguments,
                None,
                Some(&denial),
                None,
                Instant::now(),
            );
            Err(Box::new(authorization_denied(denial)))
        }
    }
}

fn finish_control(
    runtime: &OrbitRuntime,
    operation: &str,
    body: &GrantControlBody,
    caller: &orbit_common::governance::authorization::CallerCapabilities,
    started: Instant,
    result: Result<OperationGrantControlResult, orbit_core::OrbitError>,
) -> Response {
    let workspace = runtime.workspace_id().unwrap_or_default();
    let target = body
        .grant_id
        .clone()
        .unwrap_or_else(|| "active".to_string());
    let arguments = json!({
        "grant_id": body.grant_id,
        "reason": body.reason,
        "expected_revision": body.expected_revision,
    });
    match result {
        Ok(result) => {
            record_operation_audit(
                runtime,
                &workspace,
                operation,
                &result.grant.id,
                "",
                &arguments,
                Some(caller),
                None,
                None,
                started,
            );
            Json(json!({
                "grant_id": result.grant.id,
                "status": result.grant.status.as_str(),
                "revision": result.grant.revision,
                "outcome": result.outcome,
                "coordinators": result
                    .coordinators
                    .iter()
                    .map(|change| json!({ "run_id": change.run_id, "outcome": change.outcome }))
                    .collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(error) => {
            let message = error.to_string();
            record_operation_audit(
                runtime,
                &workspace,
                operation,
                &target,
                "",
                &arguments,
                Some(caller),
                None,
                Some(&message),
                started,
            );
            map_runtime_error(error)
        }
    }
}
