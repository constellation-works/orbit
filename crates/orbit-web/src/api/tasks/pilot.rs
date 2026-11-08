//! `POST /tasks/:id/pilot`: the details panel's "Run pilot" action.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::DASHBOARD_JOB_RUN;

use crate::api::routines::{authorization_denied, authorized_caller};
use crate::api::{bad_request, map_runtime_error, server_error, validate_id};
use crate::state::{DashboardState, Ws};

/// Submit `task_pilot_pipeline` in explicit mode for this one task, through the
/// same submission `orbit run job task_pilot_pipeline --input 'task_ids=[...]'`
/// makes. The run assesses the task; it never promotes or dispatches it.
///
/// Requires the operator session, like every dashboard run submission. A task
/// the pipeline's explicit mode would refuse (outside `proposed`/`backlog`,
/// admitted, already held by a pilot run) or a replica checkout is a `409`
/// whose `code` names the reason; nothing is submitted. The dashboard has no
/// full re-pilot control, so this never requests a full assessment.
pub(in crate::api) async fn pilot_task_action(
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
    let submitted = tokio::task::spawn_blocking(move || {
        runtime.submit_explicit_task_pilot(
            &id,
            Some("dashboard"),
            orbit_types::workflow::JobRunTrigger::dashboard(),
        )
    })
    .await;
    match submitted {
        Ok(Ok(submission)) => Json(submission.dashboard_view()).into_response(),
        Ok(Err(error)) => map_runtime_error(error.into()),
        Err(join_err) => server_error(orbit_core::OrbitError::Execution(format!(
            "task pilot submission panicked: {join_err}"
        ))),
    }
}
