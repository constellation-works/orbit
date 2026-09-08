//! Routine and host sweep-clock operations for the dashboard [ORB-10875].

use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::{
    AuthorizationDenial, CallerCapabilities, CallerEnvelope, DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_CLOCK_SERVICE, DASHBOARD_ROUTINE_TOGGLE, GovernedOperation, authorize,
};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::application::routines::{
    ClockStatus, RoutineStatus, RoutineStatusReport, RoutineToggleOutcome, ScheduleDisplayState,
    clock_status, set_clock_cadence, set_clock_enabled, set_routine_enabled,
};
use orbit_core::{AuditEventInsertParams, OrbitRuntime, RoutineFireRecord, RoutineFireState};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::map_runtime_error;
use crate::state::{DashboardState, Ws};

#[derive(Debug, Deserialize, Default)]
pub(super) struct OperationsQuery {
    pub(super) workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RoutineToggleRequest {
    name: String,
    source: String,
    target: String,
    host_id: String,
    expected_enabled: bool,
    enabled: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ClockAction {
    Enable,
    Disable,
    SetCadence,
}

#[derive(Debug, Deserialize)]
pub(super) struct ClockControlRequest {
    action: ClockAction,
    host_id: String,
    expected_enabled: bool,
    expected_cadence_seconds: u64,
    cadence_seconds: Option<u64>,
}

/// `GET /api/routines` — routine definition state and the independent host clock.
pub(super) async fn list_routine_health(State(state): State<DashboardState>) -> Response {
    let generated_at = Utc::now();
    let report = match routine_statuses(state.global_root()) {
        Ok(report) => report,
        Err(error) => return map_runtime_error(error),
    };
    let clock = match clock_status(state.global_root()) {
        Ok(clock) => clock,
        Err(error) => return map_runtime_error(error),
    };
    Json(report_json(&report, &clock, generated_at)).into_response()
}

/// `POST /api/routines/toggle` — atomically change one selected workspace's
/// versioned `enabled` field. Browser input never supplies a path.
pub(super) async fn toggle_routine(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Json(body): Json<RoutineToggleRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let caller = match authorized_caller(&DASHBOARD_ROUTINE_TOGGLE) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                workspace,
                "routine.toggle",
                &body.name,
                &body.host_id,
                &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
                None,
                Some(&denial),
                None,
                Instant::now(),
            );
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let report = match routine_statuses(state.global_root()) {
        Ok(report) => report,
        Err(error) => return map_runtime_error(error),
    };
    let Some(status) = report
        .statuses
        .iter()
        .find(|status| status.routine.definition.name == body.name)
    else {
        return named_entity_not_found(
            "routine_not_found",
            format!("routine '{}' was not found", body.name),
        );
    };
    if status.routine.source_workspace != body.source
        || status.routine.source_orbit_dir != runtime.shared_root()
    {
        return selection_conflict(
            "workspace_mismatch",
            format!(
                "select routine source workspace '{}' before changing '{}'",
                status.routine.source_workspace, body.name
            ),
        );
    }
    if body.host_id != report.host_id || !status.pinned_to_host {
        return selection_conflict(
            "host_mismatch",
            format!(
                "select pinned host '{}' before changing '{}'",
                report.host_id, body.name
            ),
        );
    }
    let actual_target = status.routine.definition.target.as_ref_string();
    if body.target != actual_target {
        return selection_conflict(
            "target_mismatch",
            format!("routine target changed; refresh and confirm '{actual_target}'"),
        );
    }

    let outcome = match set_routine_enabled(
        &status.routine,
        &report.host_id,
        body.expected_enabled,
        body.enabled,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let error_message = error.to_string();
            record_operation_audit(
                &runtime,
                workspace,
                "routine.toggle",
                &body.name,
                &body.host_id,
                &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
                Some(&caller),
                None,
                Some(&error_message),
                started,
            );
            return map_runtime_error(error);
        }
    };
    if let RoutineToggleOutcome::Conflict { actual_enabled } = outcome {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "routine state changed while this action was pending; refresh before retrying",
                "code": "stale_routine_state",
                "actual_enabled": actual_enabled,
            })),
        )
            .into_response();
    }
    record_operation_audit(
        &runtime,
        workspace,
        "routine.toggle",
        &body.name,
        &body.host_id,
        &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
        Some(&caller),
        None,
        None,
        started,
    );
    Json(json!({
        "name": body.name,
        "source": body.source,
        "target": body.target,
        "host_id": body.host_id,
        "enabled": body.enabled,
        "changed": outcome == RoutineToggleOutcome::Changed,
        "message": if body.enabled { "Routine enabled" } else { "Routine disabled" },
    }))
    .into_response()
}

/// `POST /api/routines/clock` — typed native-service or cadence control.
pub(super) async fn control_clock(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Ws(runtime): Ws,
    Json(body): Json<ClockControlRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace,
        Err(rejection) => return rejection.into_response(),
    };
    let governed = match body.action {
        ClockAction::Enable | ClockAction::Disable => &DASHBOARD_CLOCK_SERVICE,
        ClockAction::SetCadence => &DASHBOARD_CLOCK_CADENCE,
    };
    let operation = governed.id;
    let caller = match authorized_caller(governed) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                workspace,
                operation,
                "sweep-clock",
                &body.host_id,
                &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
                None,
                Some(&denial),
                None,
                Instant::now(),
            );
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let before = match clock_status(state.global_root()) {
        Ok(status) => status,
        Err(error) => return map_runtime_error(error),
    };
    let report = match routine_statuses(state.global_root()) {
        Ok(report) => report,
        Err(error) => return map_runtime_error(error),
    };
    if body.host_id != report.host_id {
        return selection_conflict(
            "host_mismatch",
            format!(
                "select host '{}' before changing its sweep clock",
                report.host_id
            ),
        );
    }
    if before.enabled != body.expected_enabled
        || before.configured_cadence_seconds != body.expected_cadence_seconds
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "clock state changed while this action was pending; refresh before retrying",
                "code": "stale_clock_state",
                "actual_enabled": before.enabled,
                "actual_cadence_seconds": before.configured_cadence_seconds,
            })),
        )
            .into_response();
    }

    let mutation = match body.action {
        ClockAction::Enable => set_clock_enabled(state.global_root(), true).map(|_| ()),
        ClockAction::Disable => set_clock_enabled(state.global_root(), false).map(|_| ()),
        ClockAction::SetCadence => body
            .cadence_seconds
            .ok_or_else(|| {
                orbit_core::OrbitError::InvalidInput(
                    "cadence_seconds is required for set_cadence".to_string(),
                )
            })
            .and_then(|cadence| set_clock_cadence(state.global_root(), cadence).map(|_| ())),
    };
    if let Err(error) = mutation {
        let error_message = error.to_string();
        record_operation_audit(
            &runtime,
            workspace,
            operation,
            "sweep-clock",
            &body.host_id,
            &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
            Some(&caller),
            None,
            Some(&error_message),
            started,
        );
        return map_runtime_error(error);
    }
    let after = match clock_status(state.global_root()) {
        Ok(status) => status,
        Err(error) => return map_runtime_error(error),
    };
    record_operation_audit(
        &runtime,
        workspace,
        operation,
        "sweep-clock",
        &body.host_id,
        &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
        Some(&caller),
        None,
        None,
        started,
    );
    Json(json!({
        "clock": clock_json(&after),
        "changed": before != after,
        "message": match body.action {
            ClockAction::Enable => "Sweep clock enabled",
            ClockAction::Disable => "Sweep clock paused",
            ClockAction::SetCadence => "Sweep clock cadence updated",
        },
    }))
    .into_response()
}

pub(super) fn report_json(
    report: &RoutineStatusReport,
    clock: &ClockStatus,
    generated_at: DateTime<Utc>,
) -> Value {
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "host_id": report.host_id,
        "machine_id": report.machine_id,
        "controls_authorized": authorized_caller(&DASHBOARD_ROUTINE_TOGGLE).is_ok(),
        "capabilities": {
            "routine_toggle": action_capability(&DASHBOARD_ROUTINE_TOGGLE),
            "clock_service": action_capability(&DASHBOARD_CLOCK_SERVICE),
            "clock_cadence": action_capability(&DASHBOARD_CLOCK_CADENCE),
        },
        "session_explanation": if authorized_caller(&DASHBOARD_ROUTINE_TOGGLE).is_ok() {
            "Session access: this dashboard server has operator authority. Actions also check workspace and host selection. Mint creates a task without starting delivery; bounded-window submission has separate permissions."
        } else {
            "Session access comes from the dashboard server. For deliberate operator access, restart it with ORBIT_OPERATOR=1 orbit web serve and its existing options, then reload this page. Opening a terminal does not authorize a running server. Bounded-window submission has separate permissions."
        },
        "clock": clock_json(clock),
        "routines": report.statuses.iter().map(status_json).collect::<Vec<_>>(),
        "load_errors": report.load_errors.iter().map(|e| json!({
            "source_workspace": e.source_workspace,
            "path": e.path.as_ref().map(|p| p.display().to_string()),
            "message": e.message,
        })).collect::<Vec<_>>(),
    })
}

fn status_json(status: &RoutineStatus) -> Value {
    let definition = &status.routine.definition;
    json!({
        "name": definition.name,
        "description": definition.description,
        "source": status.routine.source_workspace,
        "target": definition.target.as_ref_string(),
        "enabled": definition.enabled,
        "hosts": definition.hosts,
        "pinned_to_host": status.pinned_to_host,
        "paused_at": status.paused_at,
        "effective": status.effective(),
        "cron": definition.trigger.cron,
            "trigger": definition.trigger,
            "automation": status.automation,
        "first_observed_at": status.first_observed_at,
        "last_evaluated_slot": status.last_evaluated_slot,
        "next_due": status.next_due,
        "next_evaluation": next_evaluation_json(
            status.schedule_display_state(),
            status.next_due.clone(),
        ),
        "last_fire": status.last_fire.as_ref().map(fire_json),
    })
}

pub(super) fn next_evaluation_json(state: ScheduleDisplayState, at: Option<String>) -> Value {
    json!({
        "state": state.as_str(),
        "at": if state == ScheduleDisplayState::Waiting {
            None
        } else {
            at
        },
        "hypothetical": state.is_hypothetical(),
    })
}

pub(super) fn clock_json(clock: &ClockStatus) -> Value {
    json!({
        "provider": clock.platform,
        "configured_cadence_seconds": clock.configured_cadence_seconds,
        "effective_cadence_seconds": clock.effective_cadence_seconds,
        "enabled": clock.enabled,
        "loaded": clock.loaded,
        "running": clock.running,
        "schedulable": clock.schedulable,
        "health": if !clock.enabled { "paused" } else if clock.schedulable { "healthy" } else { "missed" },
        "health_issue": clock.health_issue,
        "last_tick_at": clock.last_tick_at,
        "next_tick_at": clock.next_tick_at,
    })
}

pub(super) fn explicit_workspace(
    query: &OperationsQuery,
) -> Result<&str, (StatusCode, Json<Value>)> {
    query
        .workspace
        .as_deref()
        .filter(|workspace| !workspace.trim().is_empty())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "select one concrete workspace before using Operations controls",
                    "code": "workspace_required",
                })),
            )
        })
}

pub(super) fn authorized_caller(
    operation: &'static GovernedOperation,
) -> Result<CallerCapabilities, AuthorizationDenial> {
    let caller = CallerCapabilities::resolve(&CallerEnvelope::from_process_env(
        &ToolSessionContext::default(),
    ));
    authorize(operation, &caller)?;
    Ok(caller)
}

/// Project the same authorization decision enforced by the mutation endpoint.
pub(super) fn action_capability(operation: &'static GovernedOperation) -> Value {
    match authorized_caller(operation) {
        Ok(_) => json!({"authorized": true, "reason": null}),
        Err(denial) => json!({
            "authorized": false,
            "reason": denial.to_string(),
        }),
    }
}

pub(super) fn authorization_denied(denial: AuthorizationDenial) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": denial.to_string(),
            "code": "authorization_denied",
            "operation": denial.operation.id,
            "provenance": denial.provenance.to_string(),
        })),
    )
        .into_response()
}

/// The named routine or auto-task does not exist: a permanent 404, so a
/// client's "409 means refresh and retry" policy does not loop on it.
pub(super) fn named_entity_not_found(code: &'static str, message: String) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error": message, "code": code})),
    )
        .into_response()
}

/// The caller's selection (workspace, host, target) no longer matches the
/// entity it is trying to change: a 409 the client resolves by refreshing.
pub(super) fn selection_conflict(code: &'static str, message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": message, "code": code})),
    )
        .into_response()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_operation_audit(
    runtime: &OrbitRuntime,
    workspace: &str,
    operation: &str,
    target: &str,
    host_id: &str,
    arguments: &Value,
    caller: Option<&CallerCapabilities>,
    denial: Option<&AuthorizationDenial>,
    failure: Option<&str>,
    started: Instant,
) {
    let status = if denial.is_some() {
        AuditEventStatus::Denied
    } else if failure.is_some() {
        AuditEventStatus::Failure
    } else {
        AuditEventStatus::Success
    };
    let capabilities = caller
        .map(|caller| caller.grants().clone())
        .unwrap_or_default();
    let provenance = caller
        .map(|caller| caller.provenance().to_string())
        .or_else(|| denial.map(|denial| denial.provenance.to_string()));
    let params = AuditEventInsertParams {
        execution_id: audit_execution_id("dashboard-operations"),
        command: "dashboard.operations".to_string(),
        subcommand: provenance,
        tool_name: None,
        target_type: Some(operation.to_string()),
        target_id: Some(target.to_string()),
        role: "operator".to_string(),
        status,
        exit_code: i32::from(status != AuditEventStatus::Success),
        duration_ms: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        working_directory: runtime
            .workspace_runtime_binding()
            .map(|binding| binding.repo_root.display().to_string())
            .unwrap_or_else(|| runtime.shared_root().display().to_string()),
        arguments_json: serde_json::to_string(arguments).ok(),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: denial
            .map(ToString::to_string)
            .or_else(|| failure.map(str::to_string)),
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
        session_id: None,
        workspace_id: Some(workspace.to_string()),
        caller_machine_id: None,
        caller_host_id: Some(host_id.to_string()),
        process_machine_id: None,
        process_host_id: Some(host_id.to_string()),
        transport: None,
        effective_capabilities: capabilities,
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    };
    if let Err(error) = runtime.record_audit_event(&params) {
        tracing::error!(operation, target, error = %error, "failed to persist dashboard operation audit");
    }
}

/// One fire attempt, enriched with coarse outcome and wall-clock duration.
pub(super) fn fire_json(fire: &RoutineFireRecord) -> Value {
    let finished = fire.state.is_terminal();
    json!({
        "slot": fire.slot,
        "attempt": fire.attempt,
        "state": fire.state.as_str(),
        "ok": fire_ok(fire.state),
        "run_id": fire.run_id,
        "detail": fire.detail,
        "started_at": fire.created_at,
        "finished_at": finished.then(|| fire.updated_at.clone()),
        "duration_ms": duration_ms(fire),
    })
}

pub(super) fn fire_ok(state: RoutineFireState) -> Option<bool> {
    match state {
        RoutineFireState::Succeeded => Some(true),
        RoutineFireState::Failed | RoutineFireState::TimedOut | RoutineFireState::Error => {
            Some(false)
        }
        RoutineFireState::Intent | RoutineFireState::Dispatched => None,
    }
}

pub(super) fn duration_ms(fire: &RoutineFireRecord) -> Option<i64> {
    if !fire.state.is_terminal() {
        return None;
    }
    let start = DateTime::parse_from_rfc3339(&fire.created_at).ok()?;
    let end = DateTime::parse_from_rfc3339(&fire.updated_at).ok()?;
    Some((end - start).num_milliseconds())
}
