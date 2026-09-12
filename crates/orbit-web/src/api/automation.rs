//! Immutable coverage evidence, scoped to the selected owner workspace.

use super::{bad_request, map_runtime_error, not_found, routines::OperationsQuery};
use crate::state::DashboardState;
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};

pub(super) async fn accepted_evidence(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Path((kind, name, batch)): Path<(String, String, String)>,
) -> Response {
    let Some(workspace) = query.workspace.as_deref() else {
        return bad_request("select a workspace".to_string());
    };
    if !matches!(kind.as_str(), "auto-task" | "routine") {
        return not_found("coverage evidence not found".to_string());
    }
    let runtime = match super::auto_tasks::resolve_workspace(&state, workspace) {
        Ok((_, runtime)) => runtime,
        Err(reason) => return not_found(reason),
    };
    let result = (|| {
        let consumer = orbit_automation::consumers::consumer_key(runtime.as_ref(), &kind, &name)?;
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
        Ok(None) => not_found("coverage evidence not found".to_string()),
        Err(error) => map_runtime_error(error),
    }
}
