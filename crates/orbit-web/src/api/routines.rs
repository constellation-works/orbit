//! Routine and machine clock operations for the dashboard [ORB-10875].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Local, Utc};
use orbit_cmd::registry_routines::routine_statuses;
use orbit_common::governance::authorization::{
    AuthorizationDenial, CallerCapabilities, CallerEnvelope, DASHBOARD_CLOCK_CADENCE,
    DASHBOARD_CLOCK_SERVICE, DASHBOARD_JOB_RUN, DASHBOARD_ROUTINE_TOGGLE, GovernedOperation,
    authorize,
};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_core::application::routines::{
    ClockStatus, RoutineStatus, RoutineStatusReport, RoutineToggleOutcome, ScheduleDisplayState,
    set_clock_cadence, set_clock_enabled, set_routine_enabled,
};
use orbit_core::{AuditEventInsertParams, OrbitRuntime, RoutineFireRecord, RoutineFireState};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{blocking, map_runtime_error};
use crate::state::{DashboardState, Ws};

#[derive(Debug, Deserialize, Default)]
pub(super) struct OperationsQuery {
    pub(super) workspace: Option<String>,
    /// List reads only: also return definitions seeded by a plugin that is
    /// off where they live, marked `plugin_inactive`. Hidden by default.
    #[serde(default)]
    pub(super) include_inactive_plugins: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct RoutineToggleRequest {
    name: String,
    source: String,
    target: String,
    /// The machine the browser acted from. Recorded in the audit event;
    /// routine definitions are machine-independent [ORB-12236], so it selects
    /// nothing.
    machine_name: String,
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
    machine_name: String,
    expected_enabled: bool,
    expected_cadence_seconds: u64,
    cadence_seconds: Option<u64>,
}

/// `GET /api/routines` — routine definition state and the independent host clock.
///
/// Clock inspection is independent of definition load: a native-manager
/// transport failure still returns routine rows, with the clock projected as
/// `health: unknown` rather than HTTP 500.
///
/// A routine seeded by a plugin that is off where it lives is omitted unless
/// `include_inactive_plugins` is set; `inactive_plugin_counts` always reports
/// how many each workspace hides.
pub(super) async fn list_routine_health(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
) -> Response {
    let generated_at = Utc::now();
    let include_inactive_plugins = query.include_inactive_plugins;
    let operator_session = state.operator_session();
    match blocking("routine health", move || {
        let report = routine_statuses(state.global_root())?;
        let clock = match state.clock_status() {
            Ok(status) => clock_json(&status),
            Err(error) => unavailable_clock_json(&error.to_string()),
        };
        Ok((report, clock))
    })
    .await
    {
        Ok((report, clock)) => Json(report_json(
            &report,
            clock,
            generated_at,
            operator_session,
            include_inactive_plugins,
        ))
        .into_response(),
        Err(response) => *response,
    }
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
    let caller = match authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                workspace,
                "routine.toggle",
                &body.name,
                &body.machine_name,
                &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
                None,
                Some(&denial),
                None,
                Instant::now(),
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let global_root = state.global_root().to_path_buf();
    let report = match blocking("routine statuses", move || routine_statuses(&global_root)).await {
        Ok(report) => report,
        Err(response) => return *response,
    };
    // A replica's owner-only routine is listed with the owner named; a
    // toggle is refused with the same reason and writes nothing.
    if let Some(owned) = report
        .owner_only
        .iter()
        .find(|owned| owned.routine.definition.name == body.name)
    {
        let refusal = json!({
            "error": owned.reason,
            "code": "owner_authority",
            "owner_machine": owned.owner_machine,
        });
        return refuse_routine_toggle(&runtime, workspace, &body, &caller, started, refusal).await;
    }
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
        let refusal = json!({
            "error": format!(
                "select routine source workspace '{}' before changing '{}'",
                status.routine.source_workspace, body.name
            ),
            "code": "workspace_mismatch",
        });
        return refuse_routine_toggle(&runtime, workspace, &body, &caller, started, refusal).await;
    }
    let actual_target = status.routine.definition.target.as_ref_string();
    if body.target != actual_target {
        return refuse_routine_toggle(
            &runtime,
            workspace,
            &body,
            &caller,
            started,
            target_mismatch(&actual_target),
        )
        .await;
    }

    let outcome = match blocking("routine toggle", {
        let routine = status.routine.clone();
        let expected_enabled = body.expected_enabled;
        let enabled = body.enabled;
        move || Ok(set_routine_enabled(&routine, expected_enabled, enabled))
    })
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => {
            let error_message = error.to_string();
            record_operation_audit(
                &runtime,
                workspace,
                "routine.toggle",
                &body.name,
                &body.machine_name,
                &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
                Some(&caller),
                None,
                Some(&error_message),
                started,
            )
            .await;
            return map_runtime_error(error);
        }
        Err(response) => return *response,
    };
    let refusal = match &outcome {
        RoutineToggleOutcome::Changed | RoutineToggleOutcome::Unchanged => None,
        RoutineToggleOutcome::Conflict { actual_enabled } => Some(json!({
            "error": "routine state changed while this action was pending; refresh before retrying",
            "code": "stale_routine_state",
            "actual_enabled": actual_enabled,
        })),
        // The definition was retargeted between selection and write.
        RoutineToggleOutcome::TargetConflict { actual_target } => {
            Some(target_mismatch(&actual_target.as_ref_string()))
        }
    };
    if let Some(refusal) = refusal {
        return refuse_routine_toggle(&runtime, workspace, &body, &caller, started, refusal).await;
    }
    record_operation_audit(
        &runtime,
        workspace,
        "routine.toggle",
        &body.name,
        &body.machine_name,
        &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    Json(json!({
        "name": body.name,
        "source": body.source,
        "target": body.target,
        "machine_name": body.machine_name,
        "enabled": body.enabled,
        "changed": outcome == RoutineToggleOutcome::Changed,
        "message": if body.enabled { "Routine enabled" } else { "Routine disabled" },
    }))
    .into_response()
}

/// The request's selected target is no longer the definition's target.
fn target_mismatch(actual_target: &str) -> Value {
    json!({
        "error": format!("routine target changed; refresh and confirm '{actual_target}'"),
        "code": "target_mismatch",
        "actual_target": actual_target,
    })
}

/// Audit an authorized toggle that was refused without writing, then answer
/// with the refusal as a 409 the client resolves by refreshing.
async fn refuse_routine_toggle(
    runtime: &Arc<OrbitRuntime>,
    workspace: &str,
    body: &RoutineToggleRequest,
    caller: &CallerCapabilities,
    started: Instant,
    refusal: Value,
) -> Response {
    let reason = format!(
        "{}: {}",
        refusal["code"].as_str().unwrap_or("conflict"),
        refusal["error"].as_str().unwrap_or_default()
    );
    record_operation_audit(
        runtime,
        workspace,
        "routine.toggle",
        &body.name,
        &body.machine_name,
        &json!({"source": body.source, "target": body.target, "enabled": body.enabled}),
        Some(caller),
        None,
        Some(&reason),
        started,
    )
    .await;
    (StatusCode::CONFLICT, Json(refusal)).into_response()
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
    let caller = match authorized_caller(governed, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                workspace,
                operation,
                "clock",
                &body.machine_name,
                &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
                None,
                Some(&denial),
                None,
                Instant::now(),
            )
            .await;
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let (before, report) = match blocking("clock status", {
        let state = state.clone();
        move || {
            let before = state.clock_status()?;
            let report = routine_statuses(state.global_root())?;
            Ok((before, report))
        }
    })
    .await
    {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    if body.machine_name != report.machine_name {
        return selection_conflict(
            "machine_mismatch",
            format!(
                "select machine '{}' before changing its clock",
                report.machine_name
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

    let mutation = match blocking("clock control", {
        let state = state.clone();
        let action = body.action;
        let cadence_seconds = body.cadence_seconds;
        move || {
            #[cfg(test)]
            if let Some(result) = state.test_clock_mutation() {
                return Ok(result);
            }
            Ok(match action {
                ClockAction::Enable => set_clock_enabled(state.global_root(), true).map(|_| ()),
                ClockAction::Disable => set_clock_enabled(state.global_root(), false).map(|_| ()),
                ClockAction::SetCadence => cadence_seconds
                    .ok_or_else(|| {
                        orbit_core::OrbitError::InvalidInput(
                            "cadence_seconds is required for set_cadence".to_string(),
                        )
                    })
                    .and_then(|cadence| {
                        set_clock_cadence(state.global_root(), cadence).map(|_| ())
                    }),
            })
        }
    })
    .await
    {
        Ok(inner) => inner,
        Err(response) => return *response,
    };
    if let Err(error) = mutation {
        let error_message = error.to_string();
        record_operation_audit(
            &runtime,
            workspace,
            operation,
            "clock",
            &body.machine_name,
            &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
            Some(&caller),
            None,
            Some(&error_message),
            started,
        )
        .await;
        return map_runtime_error(error);
    }
    record_operation_audit(
        &runtime,
        workspace,
        operation,
        "clock",
        &body.machine_name,
        &json!({"action": body.action, "cadence_seconds": body.cadence_seconds}),
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    let state_after = state.clone();
    let after = tokio::task::spawn_blocking(move || state_after.clock_status()).await;
    let (clock, changed) = match after {
        Ok(Ok(status)) => (clock_json(&status), json!(before != status)),
        Ok(Err(error)) => (unavailable_clock_json(&error.to_string()), Value::Null),
        Err(error) => (
            unavailable_clock_json(&format!("clock status after panicked: {error}")),
            Value::Null,
        ),
    };
    Json(json!({
        "clock": clock,
        "changed": changed,
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
    clock: Value,
    generated_at: DateTime<Utc>,
    operator_session: bool,
    include_inactive_plugins: bool,
) -> Value {
    let mut inactive_plugin_counts = BTreeMap::<&str, usize>::new();
    for routine in report.inactive_plugin_routines() {
        *inactive_plugin_counts
            .entry(routine.source_workspace.as_str())
            .or_default() += 1;
    }
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "machine_name": report.machine_name,
        "machine_id": report.machine_id,
        "controls_authorized": authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, operator_session).is_ok(),
        "capabilities": {
            "routine_toggle": action_capability(&DASHBOARD_ROUTINE_TOGGLE, operator_session),
            "job_run": action_capability(&DASHBOARD_JOB_RUN, operator_session),
            "clock_service": action_capability(&DASHBOARD_CLOCK_SERVICE, operator_session),
            "clock_cadence": action_capability(&DASHBOARD_CLOCK_CADENCE, operator_session),
        },
        "session_explanation": if authorized_caller(&DASHBOARD_ROUTINE_TOGGLE, operator_session).is_ok() {
            "Session access: this dashboard server has operator authority. Actions also check workspace and machine selection. Mint creates a task without starting delivery; bounded-window submission has separate permissions."
        } else {
            "Session access comes from the dashboard server. For operator access, start it with `orbit web serve --operator` (or `orbit web connect`, which does that by default) and reload this page. Opening a terminal does not authorize a running server. Bounded-window submission has separate permissions."
        },
        "clock": clock,
        "cron_zone": host_cron_zone(),
        "routines": report.statuses.iter().map(status_json).collect::<Vec<_>>(),
        "retired": report.listed_retired(include_inactive_plugins).map(|routine| json!({
            "name": routine.name,
            "source": routine.source_workspace,
            "origin": routine.origin.as_str(),
            "path": routine.path.display().to_string(),
            "target": format!("job:{}", routine.job),
            "reason": routine.reason,
            "plugin_inactive": routine.skipped,
        })).collect::<Vec<_>>(),
        "owner_only": report.owner_only.iter().map(|owned| json!({
            "name": owned.routine.definition.name,
            "source": owned.routine.source_workspace,
            "origin": owned.routine.origin.as_str(),
            "target": owned.routine.definition.target.as_ref_string(),
            "enabled": owned.routine.definition.enabled,
            "owner_machine": owned.owner_machine,
            "reason": owned.reason,
        })).collect::<Vec<_>>(),
        "inactive_plugin_counts": inactive_plugin_counts,
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
        "paused_at": status.paused_at,
        "effective": status.effective(),
        "cron": definition.trigger.cron,
            "trigger": definition.trigger,
            "automation": status.automation.as_ref().map(super::automation::summary),
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

/// The zone cron triggers are evaluated in: the host's local zone, as the
/// routine and auto-task schedulers use. `name` is the IANA name when the
/// host exposes one (`TZ`, the `/etc/localtime` link, `/etc/timezone`), else
/// null; `offset_seconds` is the current offset from UTC, so a client can
/// still label the zone when the name is unknown.
pub(super) fn host_cron_zone() -> Value {
    json!({
        "name": host_zone_name(),
        "offset_seconds": Local::now().offset().local_minus_utc(),
    })
}

fn host_zone_name() -> Option<String> {
    let tz = std::env::var("TZ").ok();
    let link = std::fs::read_link("/etc/localtime").ok();
    let file = std::fs::read_to_string("/etc/timezone").ok();
    zone_name_from_sources(
        tz.as_deref(),
        link.as_deref().and_then(std::path::Path::to_str),
        file.as_deref(),
    )
}

fn zone_name_from_sources(
    tz_env: Option<&str>,
    localtime_link: Option<&str>,
    timezone_file: Option<&str>,
) -> Option<String> {
    let plausible = |name: &str| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
    };
    let from_env = tz_env.map(|tz| tz.trim().trim_start_matches(':'));
    let from_link = localtime_link
        .and_then(|target| target.split_once("zoneinfo/"))
        .map(|(_, name)| name);
    let from_file = timezone_file.map(str::trim);
    [from_env, from_link, from_file]
        .into_iter()
        .flatten()
        .find(|name| plausible(name))
        .map(str::to_string)
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
        "health": if !clock.enabled && clock.running == Some(true) { "unhealthy" }
            else if !clock.enabled { "paused" }
            else if clock.schedulable { "healthy" }
            else { "missed" },
        "health_issue": clock.health_issue,
        "last_tick_at": clock.last_tick_at,
        "next_tick_at": clock.next_tick_at,
        "error": null,
    })
}

/// Clock inspection failed: no observed enabled/paused/missed authority.
pub(super) fn unavailable_clock_json(error: &str) -> Value {
    json!({
        "provider": Value::Null,
        "configured_cadence_seconds": Value::Null,
        "effective_cadence_seconds": Value::Null,
        "enabled": Value::Null,
        "loaded": Value::Null,
        "running": Value::Null,
        "schedulable": Value::Null,
        "health": "unknown",
        "health_issue": error,
        "last_tick_at": Value::Null,
        "next_tick_at": Value::Null,
        "error": error,
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
    operator_session: bool,
) -> Result<CallerCapabilities, AuthorizationDenial> {
    let mut session = ToolSessionContext::default();
    if operator_session {
        session.effective_capabilities.insert(McpCapability::Agent);
        session
            .effective_capabilities
            .insert(McpCapability::Operator);
    }
    let caller = CallerCapabilities::resolve(&CallerEnvelope::from_process_env(&session));
    authorize(operation, &caller)?;
    Ok(caller)
}

/// Project the same authorization decision enforced by the mutation endpoint.
pub(super) fn action_capability(
    operation: &'static GovernedOperation,
    operator_session: bool,
) -> Value {
    match authorized_caller(operation, operator_session) {
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
pub(super) async fn record_operation_audit(
    runtime: &Arc<OrbitRuntime>,
    workspace: &str,
    operation: &str,
    target: &str,
    machine_name: &str,
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
        caller_machine_name: Some(machine_name.to_string()),
        process_machine_id: None,
        process_machine_name: Some(machine_name.to_string()),
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
    // A SQLite insert can wait out the busy timeout; keep it off the async
    // worker like every other store access in this crate.
    let writer = Arc::clone(runtime);
    let result = tokio::task::spawn_blocking(move || writer.record_audit_event(&params)).await;
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error.to_string(),
        Err(join_error) => join_error.to_string(),
    };
    tracing::error!(operation, target, error = %error, "failed to persist dashboard operation audit");
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
        RoutineFireState::Skipped => None,
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
