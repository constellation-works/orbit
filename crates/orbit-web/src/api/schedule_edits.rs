//! Routine trigger and auto-task schedule edits for the dashboard.
//!
//! Each edit is the same core write the MCP surface runs (routine
//! `set_trigger`, the schedule half of `auto_task.update`), behind its own
//! dashboard operation, compare-and-set on the value the browser observed,
//! and audited like the toggles. No other definition field is editable here.

use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::{
    AuthorizationDenial, DASHBOARD_AUTO_TASK_SCHEDULE, DASHBOARD_ROUTINE_TRIGGER, GovernedOperation,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::auto_tasks::{
    AutoTaskScheduleError, AutoTaskScheduleOutcome, AutoTaskScheduleProfile,
};
use orbit_core::application::routines::{RoutineEditRefusal, edit_listed_routine_trigger};
use orbit_types::workflow::{AutoTaskSchedule, RoutineTrigger};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::auto_tasks::{auto_task_control_error, resolve_workspace_blocking};
use super::routines::{
    OperationAudit, OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
    named_entity_not_found, routine_edit_refusal_reason, routine_edit_refused,
};
use super::{blocking, map_runtime_error};
use crate::state::{DashboardState, Ws};

#[derive(Debug, Deserialize)]
pub(super) struct RoutineTriggerRequest {
    name: String,
    source: String,
    target: String,
    /// The machine the browser acted from; recorded in the audit event only.
    machine_name: String,
    expected_trigger: RoutineTrigger,
    trigger: RoutineTrigger,
}

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskScheduleRequest {
    name: String,
    expected_schedule: AutoTaskSchedule,
    schedule: AutoTaskSchedule,
}

/// Typed JSON body that keeps the raw bytes when decoding fails.
///
/// The success path is Axum's typed decoder. A data error still has the
/// bytes, so a denial can name the operation. Those bytes are not decoded
/// into [`RoutineTrigger`] or [`AutoTaskSchedule`].
pub(super) struct BufferedJson<T> {
    bytes: Bytes,
    parsed: Result<T, JsonRejection>,
}

impl<T, S> FromRequest<S> for BufferedJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = JsonRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let bytes = Bytes::from_request(Request::from_parts(parts.clone(), body), state)
            .await
            .map_err(JsonRejection::from)?;
        let parsed =
            Json::<T>::from_request(Request::from_parts(parts, Body::from(bytes.clone())), state)
                .await
                .map(|Json(value)| value);
        Ok(Self { bytes, parsed })
    }
}

/// Identity of a routine edit. Nested JSON stays an audit argument and is
/// never decoded as [`RoutineTrigger`]. `expected_trigger` is required so a
/// missing nested value is not an auditable envelope.
#[derive(Debug, Deserialize)]
struct RoutineDenialIdentity {
    name: String,
    source: String,
    target: String,
    machine_name: String,
    #[serde(rename = "expected_trigger")]
    _expected_trigger: Value,
    trigger: Value,
}

/// Identity of an auto-task schedule edit. See [`RoutineDenialIdentity`].
#[derive(Debug, Deserialize)]
struct ScheduleDenialIdentity {
    name: String,
    #[serde(rename = "expected_schedule")]
    _expected_schedule: Value,
    schedule: Value,
}

struct PreparedDenial {
    name: String,
    machine_name: String,
    arguments: Value,
}

enum ScheduleDenialRoute<'a> {
    Routine { runtime: &'a Arc<OrbitRuntime> },
    AutoTask,
}

/// A rejection that never reached a decoded body: authorize first, and map a
/// typed data error to `invalid_schedule`. The editor shows the parser's own
/// explanation; any other rejection (bad syntax, wrong content type, oversized
/// body) keeps the framework's answer.
fn refused_body(
    rejection: JsonRejection,
    operation: &'static GovernedOperation,
    state: &DashboardState,
) -> Response {
    if let Err(denial) = authorized_caller(operation, state.operator_session()) {
        return authorization_denied(denial);
    }
    schedule_rejection(rejection)
}

/// Typed decoding failed. A non-operator with a readable operation identity
/// and a workspace this route can audit gets one denied row, then the same
/// 403. An operator gets `invalid_schedule` and no audit.
async fn refused_decoded_schedule(
    rejection: JsonRejection,
    bytes: &[u8],
    operation: &'static GovernedOperation,
    state: &DashboardState,
    query: &OperationsQuery,
    route: ScheduleDenialRoute<'_>,
) -> Response {
    if let Err(denial) = authorized_caller(operation, state.operator_session()) {
        if matches!(rejection, JsonRejection::JsonDataError(_)) {
            audit_denied_schedule_identity(state, query, operation, &denial, bytes, route).await;
        }
        return authorization_denied(denial);
    }
    schedule_rejection(rejection)
}

fn schedule_rejection(rejection: JsonRejection) -> Response {
    match rejection {
        rejection @ JsonRejection::JsonDataError(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("invalid request body: {}", rejection.body_text()),
                "code": "invalid_schedule",
            })),
        )
            .into_response(),
        other => other.into_response(),
    }
}

fn denial_audit_parts(bytes: &[u8], routine: bool) -> Option<PreparedDenial> {
    if routine {
        let Json(identity) = Json::<RoutineDenialIdentity>::from_bytes(bytes).ok()?;
        Some(PreparedDenial {
            name: identity.name,
            machine_name: identity.machine_name,
            arguments: json!({
                "source": identity.source,
                "target": identity.target,
                "trigger": identity.trigger,
            }),
        })
    } else {
        let Json(identity) = Json::<ScheduleDenialIdentity>::from_bytes(bytes).ok()?;
        Some(PreparedDenial {
            name: identity.name,
            machine_name: String::new(),
            arguments: json!({"schedule": identity.schedule}),
        })
    }
}

async fn audit_denied_schedule_identity(
    state: &DashboardState,
    query: &OperationsQuery,
    operation: &'static GovernedOperation,
    denial: &AuthorizationDenial,
    bytes: &[u8],
    route: ScheduleDenialRoute<'_>,
) {
    let Ok(workspace) = explicit_workspace(query) else {
        return;
    };
    let routine = matches!(route, ScheduleDenialRoute::Routine { .. });
    let Some(prepared) = denial_audit_parts(bytes, routine) else {
        return;
    };
    let resolved;
    let runtime = match route {
        ScheduleDenialRoute::Routine { runtime } => runtime,
        ScheduleDenialRoute::AutoTask => match resolve_workspace_blocking(state, workspace).await {
            Ok(runtime) => {
                resolved = runtime;
                &resolved
            }
            Err(_) => return,
        },
    };
    let audit = OperationAudit::new(
        runtime,
        workspace,
        operation.id,
        prepared.name.as_str(),
        prepared.arguments,
    )
    .with_machine_name(&prepared.machine_name);
    audit.denied(denial, Instant::now()).await;
}

fn conflict(refusal: Value) -> Response {
    (StatusCode::CONFLICT, Json(refusal)).into_response()
}

/// `POST /api/routines/trigger` — replace one routine's trigger.
pub(super) async fn set_routine_trigger(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    body: Result<BufferedJson<RoutineTriggerRequest>, JsonRejection>,
) -> Response {
    let buffered = match body {
        Ok(buffered) => buffered,
        Err(rejection) => return refused_body(rejection, &DASHBOARD_ROUTINE_TRIGGER, &state),
    };
    let body = match buffered.parsed {
        Ok(body) => body,
        Err(rejection) => {
            return refused_decoded_schedule(
                rejection,
                &buffered.bytes,
                &DASHBOARD_ROUTINE_TRIGGER,
                &state,
                &query,
                ScheduleDenialRoute::Routine { runtime: &runtime },
            )
            .await;
        }
    };
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let audit = OperationAudit::new(
        &runtime,
        &workspace,
        DASHBOARD_ROUTINE_TRIGGER.id,
        body.name.as_str(),
        json!({"source": body.source, "target": body.target, "trigger": body.trigger}),
    )
    .with_machine_name(&body.machine_name);
    let caller = match authorized_caller(&DASHBOARD_ROUTINE_TRIGGER, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            audit.denied(&denial, Instant::now()).await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let selected = match blocking("routine trigger edit", {
        let global_root = state.global_root().to_path_buf();
        let shared_root = runtime.shared_root();
        let (name, source, target) = (body.name.clone(), body.source.clone(), body.target.clone());
        let (expected, trigger) = (body.expected_trigger.clone(), body.trigger.clone());
        move || {
            let report = routine_statuses(&global_root)?;
            Ok(edit_listed_routine_trigger(
                &report,
                &name,
                Some((&source, &shared_root)),
                &target,
                &expected,
                trigger,
            ))
        }
    })
    .await
    {
        Ok(selected) => selected,
        Err(response) => return *response,
    };
    let changed = match selected {
        Ok(Ok(outcome)) => outcome.changed,
        // An unlisted routine is a 404 with nothing to audit.
        Ok(Err(refusal @ RoutineEditRefusal::NotFound { .. })) => {
            return routine_edit_refused(&refusal);
        }
        Ok(Err(refusal)) => {
            let reason = routine_edit_refusal_reason(&refusal);
            audit.failed(&caller, &reason, started).await;
            return routine_edit_refused(&refusal);
        }
        Err(error) => {
            audit.failed(&caller, &error.to_string(), started).await;
            return map_runtime_error(error);
        }
    };
    audit.succeeded(&caller, started).await;
    Json(json!({
        "name": body.name,
        "target": body.target,
        "trigger": body.trigger,
        "changed": changed,
        "message": if changed { "Routine trigger updated" } else { "Routine trigger already matched" },
    }))
    .into_response()
}

/// `POST /api/auto-tasks/schedule` — replace one auto-task's schedule.
pub(super) async fn set_auto_task_schedule(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    body: Result<BufferedJson<AutoTaskScheduleRequest>, JsonRejection>,
) -> Response {
    let buffered = match body {
        Ok(buffered) => buffered,
        Err(rejection) => return refused_body(rejection, &DASHBOARD_AUTO_TASK_SCHEDULE, &state),
    };
    let body = match buffered.parsed {
        Ok(body) => body,
        Err(rejection) => {
            return refused_decoded_schedule(
                rejection,
                &buffered.bytes,
                &DASHBOARD_AUTO_TASK_SCHEDULE,
                &state,
                &query,
                ScheduleDenialRoute::AutoTask,
            )
            .await;
        }
    };
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    let audit = OperationAudit::new(
        &runtime,
        &workspace,
        DASHBOARD_AUTO_TASK_SCHEDULE.id,
        body.name.as_str(),
        json!({"schedule": body.schedule}),
    );
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_SCHEDULE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            audit.denied(&denial, Instant::now()).await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let outcome = match blocking("auto-task schedule", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        let schedule = body.schedule.clone();
        let profile = AutoTaskScheduleProfile::Observed {
            expected_schedule: Box::new(body.expected_schedule.clone()),
        };
        move || Ok(runtime.auto_task_schedule_command(&name, schedule, profile))
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    let updated = match outcome {
        Ok(AutoTaskScheduleOutcome::Written(updated)) => updated,
        Ok(AutoTaskScheduleOutcome::AlreadySet(current)) => {
            return Json(json!({
                "name": body.name,
                "schedule": current.schedule,
                "changed": false,
                "message": "Auto-task schedule already matched",
            }))
            .into_response();
        }
        Ok(AutoTaskScheduleOutcome::Stale { actual_schedule }) => {
            return conflict(json!({
                "error": "auto-task schedule changed while this edit was open; refresh before retrying",
                "code": "stale_auto_task_schedule",
                "actual_schedule": actual_schedule,
            }));
        }
        Ok(AutoTaskScheduleOutcome::NotFound) => {
            return named_entity_not_found(
                "auto_task_not_found",
                format!("auto-task '{}' was not found", body.name),
            );
        }
        Err(AutoTaskScheduleError::Read(error)) => return map_runtime_error(error),
        Err(AutoTaskScheduleError::Write(error)) => {
            audit.failed(&caller, &error.to_string(), started).await;
            return auto_task_control_error(error);
        }
    };
    audit.succeeded(&caller, started).await;
    Json(json!({
        "name": updated.name,
        "schedule": updated.schedule,
        "changed": true,
        "message": "Auto-task schedule updated",
    }))
    .into_response()
}
