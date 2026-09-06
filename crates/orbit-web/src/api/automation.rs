//! Immutable coverage evidence, scoped to the selected owner workspace.
use super::{map_runtime_error, routines::OperationsQuery};
use crate::state::DashboardState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub(super) async fn accepted_evidence(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Path((kind, name, batch)): Path<(String, String, String)>,
) -> Response {
    let Some(workspace) = query.workspace.as_deref() else {
        return (StatusCode::BAD_REQUEST, "select a workspace").into_response();
    };
    if !matches!(kind.as_str(), "auto-task" | "routine") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let runtime = match super::auto_tasks::resolve_workspace(&state, workspace) {
        Ok((_, runtime)) => runtime,
        Err(reason) => return (StatusCode::NOT_FOUND, reason).into_response(),
    };
    let result = (|| {
        let consumer = orbit_core::application::automation::consumer_key(&runtime, &kind, &name)?;
        runtime
            .automation_store()?
            .automation_receipt(&consumer, &batch)
    })();
    match result {
        Ok(Some(receipt)) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            receipt.evidence,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => map_runtime_error(error),
    }
}
