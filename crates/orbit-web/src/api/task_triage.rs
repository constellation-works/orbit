//! `POST /tasks/:id/triage`: the details panel's "Triage" action.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_JOB_RUN;
use orbit_core::application::job::EscalationTriageSubmitError;
use serde_json::json;

use super::routines::{authorization_denied, authorized_caller};
use super::{bad_request, map_runtime_error, server_error, validate_id};
use crate::state::{DashboardState, Ws};

/// Submit `escalation_triage_pipeline` in explicit mode for this one task,
/// through the same submission `orbit run job escalation_triage_pipeline
/// --input 'task_ids=[...]'` makes, with the subject the task's status
/// selects (`pilot_warning` for a proposed task, `block` for a blocked
/// one). The run only proposes; it never approves, promotes, clears a hold
/// or edits the task itself.
///
/// Requires the operator session, like every dashboard run submission. A
/// task that does not currently qualify (already triaged for this
/// assessment or episode, decided or touched by an operator since, held by
/// a live triage run, or a block with no escalated or overdue recovery
/// decision) or a replica checkout is a `409` whose `code` names the
/// reason; nothing is submitted.
pub(super) async fn triage_task_action(
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
        runtime.submit_explicit_escalation_triage(
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
        Ok(Err(EscalationTriageSubmitError::Refused(refusal))) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": refusal.to_string(),
                "code": refusal.code(),
                "reason": refusal.reason(),
            })),
        )
            .into_response(),
        Ok(Err(EscalationTriageSubmitError::Runtime(error))) => map_runtime_error(error),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task triage submission panicked: {join_err}"
        ))),
    }
}
