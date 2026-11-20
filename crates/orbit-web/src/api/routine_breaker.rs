//! Routine circuit-breaker controls for the dashboard [ORB-25103]: reset a
//! tripped routine now, or let it fire once on the next scheduler tick. The
//! breaker is host-local, like a pause; both actions are operator-only and
//! audited, and the store also records them in the routine's breaker log.

use std::time::Instant;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::DASHBOARD_ROUTINE_BREAKER;
use orbit_core::application::routines::{
    RoutineBreakerAction as CoreBreakerAction, RoutineEditRefusal, control_listed_routine_breaker,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::blocking;
use super::routines::{
    OperationAudit, OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
    named_entity_not_found, routine_edit_refusal_reason, routine_edit_refused, selection_conflict,
};
use crate::state::{DashboardState, Ws};

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
pub(super) async fn control_routine_breaker(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Json(body): Json<RoutineBreakerRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let audit = OperationAudit::new(
        &runtime,
        workspace,
        DASHBOARD_ROUTINE_BREAKER.id,
        body.name.as_str(),
        json!({"source": body.source, "action": body.action}),
    )
    .with_machine_name(&body.machine_name);
    let caller = match authorized_caller(&DASHBOARD_ROUTINE_BREAKER, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            audit.denied(&denial, Instant::now()).await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let global_root = state.global_root().to_path_buf();
    let (name, source, action) = (body.name.clone(), body.source.clone(), body.action);
    let outcome = match blocking("routine breaker", move || {
        let report = routine_statuses(&global_root)?;
        let action = match action {
            RoutineBreakerAction::Reset => CoreBreakerAction::Reset,
            RoutineBreakerAction::Bypass => CoreBreakerAction::Bypass,
        };
        // The request names the source workspace but not its `.orbit` directory.
        control_listed_routine_breaker(
            &global_root,
            &report,
            &name,
            Some((&source, None)),
            action,
            "dashboard",
        )
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => {
            audit
                .failed(&caller, "routine breaker update failed", started)
                .await;
            return *response;
        }
    };
    let changed = match outcome {
        Ok(outcome) => outcome.changed(),
        // An unlisted routine is a 404 with nothing to audit.
        Err(RoutineEditRefusal::NotFound { .. }) => {
            return named_entity_not_found(
                "routine_not_found",
                format!("no routine named '{}' on this host", body.name),
            );
        }
        // Owner authority and a source mismatch wrote nothing; audit and answer 409.
        Err(refusal) => {
            audit
                .failed(&caller, &routine_edit_refusal_reason(&refusal), started)
                .await;
            return routine_edit_refused(&refusal);
        }
    };
    if !changed {
        let message = format!("the breaker of '{}' is not tripped on this host", body.name);
        audit.failed(&caller, &message, started).await;
        return selection_conflict("routine_breaker_closed", message);
    }
    audit.succeeded(&caller, started).await;
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
