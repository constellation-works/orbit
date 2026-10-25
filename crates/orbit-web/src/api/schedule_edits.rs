//! Routine trigger and auto-task schedule edits for the dashboard.
//!
//! Each edit is the same core write the MCP surface runs (routine
//! `set_trigger`, the schedule half of `auto_task.update`), behind its own
//! dashboard operation, compare-and-set on the value the browser observed,
//! and audited like the toggles. No other definition field is editable here.

use std::time::Instant;

use axum::extract::{Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_SCHEDULE, DASHBOARD_ROUTINE_TRIGGER,
};
use orbit_core::application::routines::{
    RoutineToggleRefusal, RoutineTriggerEditOutcome, edit_listed_routine_trigger,
};
use orbit_types::workflow::{AutoTaskSchedule, RoutineTrigger};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::auto_tasks::{auto_task_control_error, resolve_workspace_blocking};
use super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
    named_entity_not_found, record_operation_audit,
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
    expected_trigger: Value,
    trigger: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskScheduleRequest {
    name: String,
    expected_schedule: Value,
    schedule: Value,
}

/// Parse one typed field of a request body, refusing a malformed value with
/// the parser's own explanation as a 400 the editor shows verbatim.
fn typed<T: DeserializeOwned>(value: &Value, field: &str, what: &str) -> Result<T, Box<Response>> {
    serde_json::from_value(value.clone()).map_err(|error| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!("{field} is not a valid {what}: {error}"),
                    "code": "invalid_schedule",
                })),
            )
                .into_response(),
        )
    })
}

fn conflict(refusal: Value) -> Response {
    (StatusCode::CONFLICT, Json(refusal)).into_response()
}

/// `POST /api/routines/trigger` — replace one routine's trigger.
pub(super) async fn set_routine_trigger(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    body: Result<Json<RoutineTriggerRequest>, JsonRejection>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    // Authorize before touching the body so unauthenticated probes get 403,
    // not the 422 that JSON extraction would return for a malformed body.
    let caller = match authorized_caller(&DASHBOARD_ROUTINE_TRIGGER, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            let started = Instant::now();
            match &body {
                Ok(Json(valid)) => {
                    record_operation_audit(
                        &runtime,
                        &workspace,
                        DASHBOARD_ROUTINE_TRIGGER.id,
                        &valid.name,
                        &valid.machine_name,
                        &json!({"source": valid.source, "target": valid.target, "trigger": valid.trigger}),
                        None,
                        Some(&denial),
                        None,
                        started,
                    )
                    .await;
                }
                Err(_) => {
                    record_operation_audit(
                        &runtime,
                        &workspace,
                        DASHBOARD_ROUTINE_TRIGGER.id,
                        "",
                        "",
                        &json!({}),
                        None,
                        Some(&denial),
                        None,
                        started,
                    )
                    .await;
                }
            }
            return authorization_denied(denial);
        }
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let audit_args = json!({"source": body.source, "target": body.target, "trigger": body.trigger});
    let audit = |caller, denial, failure: Option<String>, started| {
        let runtime = runtime.clone();
        let (workspace, name, machine, args) = (
            workspace.clone(),
            body.name.clone(),
            body.machine_name.clone(),
            audit_args.clone(),
        );
        async move {
            record_operation_audit(
                &runtime,
                &workspace,
                DASHBOARD_ROUTINE_TRIGGER.id,
                &name,
                &machine,
                &args,
                caller,
                denial,
                failure.as_deref(),
                started,
            )
            .await;
        }
    };
    let expected = match typed::<RoutineTrigger>(
        &body.expected_trigger,
        "expected_trigger",
        "routine trigger",
    ) {
        Ok(trigger) => trigger,
        Err(response) => return *response,
    };
    let trigger = match typed::<RoutineTrigger>(&body.trigger, "trigger", "routine trigger") {
        Ok(trigger) => trigger,
        Err(response) => return *response,
    };
    let started = Instant::now();
    let selected = match blocking("routine trigger edit", {
        let global_root = state.global_root().to_path_buf();
        let shared_root = runtime.shared_root();
        let (name, source, target) = (body.name.clone(), body.source.clone(), body.target.clone());
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
    let changed = matches!(selected, Ok(Ok(RoutineTriggerEditOutcome::Changed)));
    let refusal = match selected {
        Ok(Ok(RoutineTriggerEditOutcome::Changed | RoutineTriggerEditOutcome::Unchanged)) => None,
        Ok(Ok(RoutineTriggerEditOutcome::Conflict { actual_trigger })) => Some(json!({
            "error": "routine trigger changed while this edit was open; refresh before retrying",
            "code": "stale_routine_trigger",
            "actual_trigger": actual_trigger,
        })),
        Ok(Ok(RoutineTriggerEditOutcome::TargetConflict { actual_target })) => Some(json!({
            "error": format!("routine target changed; refresh and confirm '{}'", actual_target.as_ref_string()),
            "code": "target_mismatch",
            "actual_target": actual_target.as_ref_string(),
        })),
        Ok(Err(RoutineToggleRefusal::OwnerAuthority {
            reason,
            owner_machine,
        })) => Some(json!({
            "error": reason,
            "code": "owner_authority",
            "owner_machine": owner_machine,
        })),
        Ok(Err(RoutineToggleRefusal::NotFound)) => {
            return named_entity_not_found(
                "routine_not_found",
                format!("routine '{}' was not found", body.name),
            );
        }
        Ok(Err(RoutineToggleRefusal::WorkspaceMismatch { source_workspace })) => Some(json!({
            "error": format!(
                "select routine source workspace '{source_workspace}' before changing '{}'",
                body.name
            ),
            "code": "workspace_mismatch",
        })),
        Err(error) => {
            audit(Some(&caller), None, Some(error.to_string()), started).await;
            return map_runtime_error(error);
        }
    };
    if let Some(refusal) = refusal {
        let reason = format!(
            "{}: {}",
            refusal["code"].as_str().unwrap_or("conflict"),
            refusal["error"].as_str().unwrap_or_default()
        );
        audit(Some(&caller), None, Some(reason), started).await;
        return conflict(refusal);
    }
    audit(Some(&caller), None, None, started).await;
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
    body: Result<Json<AutoTaskScheduleRequest>, JsonRejection>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    // Authorize before touching the body so unauthenticated probes get 403,
    // not the 422 that JSON extraction would return for a malformed body.
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_SCHEDULE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            let started = Instant::now();
            match &body {
                Ok(Json(valid)) => {
                    record_operation_audit(
                        &runtime,
                        &workspace,
                        DASHBOARD_AUTO_TASK_SCHEDULE.id,
                        &valid.name,
                        "",
                        &json!({"schedule": valid.schedule}),
                        None,
                        Some(&denial),
                        None,
                        started,
                    )
                    .await;
                }
                Err(_) => {
                    record_operation_audit(
                        &runtime,
                        &workspace,
                        DASHBOARD_AUTO_TASK_SCHEDULE.id,
                        "",
                        "",
                        &json!({}),
                        None,
                        Some(&denial),
                        None,
                        started,
                    )
                    .await;
                }
            }
            return authorization_denied(denial);
        }
    };
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.into_response(),
    };
    let audit_args = json!({"schedule": body.schedule});
    let audit = |caller, denial, failure: Option<String>, started| {
        let runtime = runtime.clone();
        let (workspace, name, args) = (workspace.clone(), body.name.clone(), audit_args.clone());
        async move {
            record_operation_audit(
                &runtime,
                &workspace,
                DASHBOARD_AUTO_TASK_SCHEDULE.id,
                &name,
                "",
                &args,
                caller,
                denial,
                failure.as_deref(),
                started,
            )
            .await;
        }
    };
    let expected = match typed::<AutoTaskSchedule>(
        &body.expected_schedule,
        "expected_schedule",
        "auto-task schedule",
    ) {
        Ok(schedule) => schedule,
        Err(response) => return *response,
    };
    let schedule = match typed::<AutoTaskSchedule>(&body.schedule, "schedule", "auto-task schedule")
    {
        Ok(schedule) => schedule,
        Err(response) => return *response,
    };
    let started = Instant::now();
    let current = match blocking("auto-task show", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        move || runtime.auto_task_show(&name)
    })
    .await
    {
        Ok(Some(definition)) => definition,
        Ok(None) => {
            return named_entity_not_found(
                "auto_task_not_found",
                format!("auto-task '{}' was not found", body.name),
            );
        }
        Err(response) => return *response,
    };
    if current.schedule != expected {
        return conflict(json!({
            "error": "auto-task schedule changed while this edit was open; refresh before retrying",
            "code": "stale_auto_task_schedule",
            "actual_schedule": current.schedule,
        }));
    }
    if current.schedule == schedule {
        return Json(json!({
            "name": body.name,
            "schedule": current.schedule,
            "changed": false,
            "message": "Auto-task schedule already matched",
        }))
        .into_response();
    }
    let updated = match blocking("auto-task schedule", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        move || Ok(runtime.auto_task_set_schedule_checked(&name, &expected, schedule))
    })
    .await
    {
        Ok(Ok(updated)) => updated,
        Ok(Err(error)) => {
            audit(Some(&caller), None, Some(error.to_string()), started).await;
            return auto_task_control_error(error);
        }
        Err(response) => return *response,
    };
    audit(Some(&caller), None, None, started).await;
    Json(json!({
        "name": updated.name,
        "schedule": updated.schedule,
        "changed": true,
        "message": "Auto-task schedule updated",
    }))
    .into_response()
}
