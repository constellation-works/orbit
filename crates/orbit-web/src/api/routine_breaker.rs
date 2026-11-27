//! Routine circuit-breaker controls for the dashboard [ORB-25103]: reset a
//! tripped routine now, or let it fire once on the next scheduler tick. The
//! breaker is host-local, like a pause; both actions are operator-only and
//! audited by the routine app, and the store also records them in the
//! routine's breaker log.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::app::routine::{
    DashboardWrite, DashboardWritten, RoutineBreakerAction as CoreBreakerAction, RoutineEditRefusal,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::map_runtime_error;
use super::routines::{
    OperationsQuery, dashboard_caller, explicit_workspace, named_entity_not_found,
    routine_edit_refused, routine_write, selection_conflict, statuses_read, unexpected_outcome,
};
use crate::state::{DashboardState, WsCore};

/// The two operator actions on a tripped routine.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum RoutineBreakerAction {
    /// Close the breaker now.
    Reset,
    /// Fire once on the next tick.
    Bypass,
}

#[derive(Debug, Deserialize)]
pub(super) struct RoutineBreakerRequest {
    name: String,
    source: String,
    action: RoutineBreakerAction,
    /// The machine the browser acted from, recorded in the audit event.
    machine_name: String,
}

/// `POST /api/routines/breaker` — reset or bypass one routine's breaker.
///
/// The routine app authorizes, controls and audits: an unlisted routine is a
/// 404 with nothing to audit; owner authority, a source mismatch and a
/// breaker that is not tripped wrote nothing, are audited and answer 409.
pub(super) async fn control_routine_breaker(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    Json(body): Json<RoutineBreakerRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let written = routine_write(
        "routine breaker",
        core,
        dashboard_caller(&state, Some(workspace), &body.machine_name),
        DashboardWrite::Breaker {
            name: body.name.clone(),
            source: body.source.clone(),
            action: match body.action {
                RoutineBreakerAction::Reset => CoreBreakerAction::Reset,
                RoutineBreakerAction::Bypass => CoreBreakerAction::Bypass,
            },
            global_root: state.global_root().to_path_buf(),
            opener: orbit_core::app::workspace::store_opener(),
            statuses: statuses_read(&state),
        },
    )
    .await;
    let changed = match written {
        Ok(DashboardWritten::Breaker(Ok(Ok(outcome)))) => outcome.changed(),
        Ok(DashboardWritten::Breaker(Ok(Err(RoutineEditRefusal::NotFound { .. })))) => {
            return named_entity_not_found(
                "routine_not_found",
                format!("no routine named '{}' on this host", body.name),
            );
        }
        Ok(DashboardWritten::Breaker(Ok(Err(refusal)))) => return routine_edit_refused(&refusal),
        Ok(DashboardWritten::Breaker(Err(error))) => return map_runtime_error(error),
        Ok(_) => return unexpected_outcome("routine breaker"),
        Err(response) => return *response,
    };
    if !changed {
        let message = format!("the breaker of '{}' is not tripped on this host", body.name);
        return selection_conflict("routine_breaker_closed", message);
    }
    Json(json!({
        "name": body.name,
        "source": body.source,
        "action": body.action,
        "changed": changed,
        "message": match body.action {
            RoutineBreakerAction::Reset => "Breaker reset",
            RoutineBreakerAction::Bypass => "Fires once on the next tick",
        },
    }))
    .into_response()
}
