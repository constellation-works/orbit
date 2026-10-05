//! Immutable coverage evidence, scoped to the selected owner workspace.

use super::{bad_request, blocking, not_found, routines::OperationsQuery};
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
    let workspace = workspace.to_string();
    match blocking("coverage evidence", move || {
        let runtime = match super::auto_tasks::resolve_workspace(&state, &workspace) {
            Ok((_, runtime)) => runtime,
            Err(reason) => return Ok(EvidenceLookup::UnknownWorkspace(reason)),
        };
        match (|| {
            let consumer =
                orbit_core::application::automation::consumer_key(&runtime, &kind, &name)?;
            runtime
                .automation_store()?
                .automation_receipt(&consumer, &batch)
        })() {
            Ok(Some(receipt)) => Ok(EvidenceLookup::Body(receipt.evidence)),
            Ok(None) => Ok(EvidenceLookup::Missing),
            Err(error) => Err(error),
        }
    })
    .await
    {
        Ok(EvidenceLookup::Body(evidence)) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            evidence,
        )
            .into_response(),
        Ok(EvidenceLookup::Missing) => not_found("coverage evidence not found".to_string()),
        Ok(EvidenceLookup::UnknownWorkspace(reason)) => not_found(reason),
        Err(response) => *response,
    }
}

enum EvidenceLookup {
    Body(Vec<u8>),
    Missing,
    UnknownWorkspace(String),
}
