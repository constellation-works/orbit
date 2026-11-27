//! Routine trigger and auto-task schedule edits for the dashboard.
//!
//! Each edit is the same core write the MCP surface runs (routine
//! `set_trigger`, the schedule half of `auto_task.update`), behind its own
//! dashboard operation, compare-and-set on the value the browser observed,
//! and audited like the toggles. The routine and auto-task apps authorize,
//! edit and audit; these handlers parse the request and render the outcome.
//! No other definition field is editable here.

use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_SCHEDULE, DASHBOARD_ROUTINE_TRIGGER, GovernedOperation,
};
use orbit_core::app::auto_task::{self, AutoTaskScheduleError, AutoTaskScheduleOutcome};
use orbit_core::app::routine;
use orbit_types::workflow::{AutoTaskSchedule, RoutineTrigger};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::auto_tasks::{auto_task_control_error, auto_task_write, resolve_workspace_blocking};
use super::map_runtime_error;
use super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, dashboard_caller, explicit_workspace,
    named_entity_not_found, routine_edit_refused, routine_write, statuses_read, unexpected_outcome,
};
use crate::state::{DashboardState, WsCore};

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

/// The operation identity of a body whose typed decoding failed on its data,
/// when the request also names a workspace: what a denial can be audited
/// under. Anything else is answered by [`refused_body`], with no audit.
fn undecoded_identity<'a>(
    rejection: &JsonRejection,
    bytes: &[u8],
    query: &'a OperationsQuery,
    routine: bool,
) -> Option<(&'a str, PreparedDenial)> {
    if !matches!(rejection, JsonRejection::JsonDataError(_)) {
        return None;
    }
    let workspace = explicit_workspace(query).ok()?;
    Some((workspace, denial_audit_parts(bytes, routine)?))
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

fn conflict(refusal: Value) -> Response {
    (StatusCode::CONFLICT, Json(refusal)).into_response()
}

/// `POST /api/routines/trigger` — replace one routine's trigger.
///
/// The routine app authorizes, edits and audits, with the same refusal
/// rules as the toggle. A body that names its routine but does not decode
/// reaches the app too: a session that may not edit triggers gets one denied
/// row and a 403, and an operator gets `invalid_schedule` and no audit.
pub(super) async fn set_routine_trigger(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    WsCore(core): WsCore,
    body: Result<BufferedJson<RoutineTriggerRequest>, JsonRejection>,
) -> Response {
    let buffered = match body {
        Ok(buffered) => buffered,
        Err(rejection) => return refused_body(rejection, &DASHBOARD_ROUTINE_TRIGGER, &state),
    };
    let body = match buffered.parsed {
        Ok(body) => body,
        Err(rejection) => {
            let Some((workspace, prepared)) =
                undecoded_identity(&rejection, &buffered.bytes, &query, true)
            else {
                return refused_body(rejection, &DASHBOARD_ROUTINE_TRIGGER, &state);
            };
            return match routine_write(
                "routine trigger edit",
                core,
                dashboard_caller(&state, Some(workspace), &prepared.machine_name),
                routine::DashboardWrite::UndecodedTrigger {
                    name: prepared.name,
                    arguments: prepared.arguments,
                },
            )
            .await
            {
                Ok(_) => schedule_rejection(rejection),
                Err(response) => *response,
            };
        }
    };
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let written = routine_write(
        "routine trigger edit",
        core,
        dashboard_caller(&state, Some(workspace), &body.machine_name),
        routine::DashboardWrite::Trigger {
            routine: routine::ListedRoutine {
                name: body.name.clone(),
                source: body.source.clone(),
                target: body.target.clone(),
            },
            expected: Box::new(body.expected_trigger.clone()),
            trigger: Box::new(body.trigger.clone()),
            statuses: statuses_read(&state),
        },
    )
    .await;
    let changed = match written {
        Ok(routine::DashboardWritten::Edited(Ok(Ok(outcome)))) => outcome.changed,
        Ok(routine::DashboardWritten::Edited(Ok(Err(refusal)))) => {
            return routine_edit_refused(&refusal);
        }
        Ok(routine::DashboardWritten::Edited(Err(error))) => return map_runtime_error(error),
        Ok(_) => return unexpected_outcome("routine trigger edit"),
        Err(response) => return *response,
    };
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
///
/// The auto-task app authorizes, edits and audits an attempted write. An
/// undecoded body is handled as [`set_routine_trigger`] handles one, when its
/// workspace resolves.
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
            let Some((workspace, prepared)) =
                undecoded_identity(&rejection, &buffered.bytes, &query, false)
            else {
                return refused_body(rejection, &DASHBOARD_AUTO_TASK_SCHEDULE, &state);
            };
            let Ok(core) = resolve_workspace_blocking(&state, workspace).await else {
                return refused_body(rejection, &DASHBOARD_AUTO_TASK_SCHEDULE, &state);
            };
            return match auto_task_write(
                "auto-task schedule",
                core,
                dashboard_caller(&state, Some(workspace), ""),
                auto_task::DashboardWrite::UndecodedSchedule {
                    name: prepared.name,
                    arguments: prepared.arguments,
                },
            )
            .await
            {
                Ok(_) => schedule_rejection(rejection),
                Err(response) => *response,
            };
        }
    };
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let core = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(core) => core,
        Err(response) => return *response,
    };
    let written = auto_task_write(
        "auto-task schedule",
        core,
        dashboard_caller(&state, Some(&workspace), ""),
        auto_task::DashboardWrite::Schedule {
            name: body.name.clone(),
            expected_schedule: Box::new(body.expected_schedule.clone()),
            schedule: Box::new(body.schedule.clone()),
        },
    )
    .await;
    let outcome = match written {
        Ok(auto_task::DashboardWritten::Scheduled(outcome)) => outcome,
        Ok(_) => return unexpected_outcome("auto-task schedule"),
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
        Err(AutoTaskScheduleError::Write(error)) => return auto_task_control_error(error),
    };
    Json(json!({
        "name": updated.name,
        "schedule": updated.schedule,
        "changed": true,
        "message": "Auto-task schedule updated",
    }))
    .into_response()
}
