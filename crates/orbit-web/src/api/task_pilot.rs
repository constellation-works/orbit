//! `POST /tasks/:id/pilot`: the details panel's "Run pilot" action.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_JOB_RUN;
use orbit_core::application::job::TaskPilotSubmitError;
use serde_json::json;

use super::routines::{authorization_denied, authorized_caller};
use super::{bad_request, map_runtime_error, server_error, validate_id};
use crate::state::{DashboardState, Ws};

/// Submit `task_pilot_pipeline` in explicit mode for this one task, through the
/// same submission `orbit run job task_pilot_pipeline --input 'task_ids=[...]'`
/// makes. The run assesses the task; it never promotes or dispatches it.
///
/// Requires the operator session, like every dashboard run submission. A task
/// the pipeline's explicit mode would refuse (outside `proposed`/`backlog`,
/// admitted, already held by a pilot run) or a replica checkout is a `409`
/// whose `code` names the reason; nothing is submitted.
pub(super) async fn pilot_task_action(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    Path(id): Path<String>,
) -> Response {
    if let Err(denial) = authorized_caller(&DASHBOARD_JOB_RUN, state.operator_session()) {
        return authorization_denied(denial);
    }
    let id = match validate_id(&id) {
        Ok(id) => id.to_string(),
        Err(message) => return bad_request(message),
    };
    let task_id = id.clone();
    let submitted = tokio::task::spawn_blocking(move || {
        runtime.submit_explicit_task_pilot(
            &id,
            Some("dashboard"),
            orbit_types::workflow::JobRunTrigger::dashboard(),
        )
    })
    .await;
    match submitted {
        Ok(Ok(invoke)) => Json(json!({
            "task_id": task_id,
            "job_id": invoke.job_name,
            "run_id": invoke.run_id,
            "state": if invoke.queued { "queued" } else { "submitted" },
            "submitted_at": invoke.submitted_at,
        }))
        .into_response(),
        Ok(Err(TaskPilotSubmitError::Refused(refusal))) => (
            StatusCode::CONFLICT,
            Json(json!({"error": refusal.to_string(), "code": refusal.code()})),
        )
            .into_response(),
        Ok(Err(TaskPilotSubmitError::Runtime(error))) => map_runtime_error(error),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task pilot submission panicked: {join_err}"
        ))),
    }
}
