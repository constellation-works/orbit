//! Auto-task inspection, toggle, manual mint, and delete for the dashboard [ORB-10876].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_DELETE, DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_SCHEDULE,
    DASHBOARD_AUTO_TASK_TOGGLE,
};
use orbit_core::application::auto_tasks::delete::{self, AutoTaskDeleteProfile};
use orbit_core::application::auto_tasks::projection::AutoTaskListingProfile;
use orbit_core::application::auto_tasks::{
    AutoTaskDeleteParams, AutoTaskToggleError, AutoTaskToggleOutcome, AutoTaskToggleProfile,
};
use orbit_core::{OrbitRuntime, Task};
use orbit_types::workflow::auto_task_tag;
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use super::map_runtime_error;
use super::routines::{
    OperationsQuery, action_capability, authorization_denied, authorized_caller,
    explicit_workspace, host_cron_zone, named_entity_not_found, record_operation_audit,
    selection_conflict,
};
use crate::state::DashboardState;

const UNCONDITIONAL_MINT_WARNING: &str = "Manual mint ignores this definition's schedule, \
enabled flag, and scheduler dedupe policy. It does not read or write the host-local cursor.";

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskToggleRequest {
    name: String,
    expected_enabled: bool,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskMintRequest {
    name: String,
    #[serde(default)]
    acknowledge_unconditional: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskDeleteRequest {
    name: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    force: bool,
}

/// `GET /api/auto-tasks` — workspace-scoped definition state.
pub(super) async fn list_auto_tasks(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
) -> Response {
    let generated_at = Utc::now();
    let Some(workspace) = query
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|workspace| !workspace.is_empty())
    else {
        return Json(read_only_envelope(
            generated_at,
            None,
            "All-workspace mode is read-only. Select one concrete workspace; auto-task definitions are workspace-scoped.",
        ))
        .into_response();
    };
    let workspace = workspace.to_string();
    let include_inactive_plugins = query.include_inactive_plugins;
    match blocking("auto-task list", {
        let state = state.clone();
        let workspace = workspace.clone();
        move || {
            Ok(match resolve_workspace(&state, &workspace) {
                Ok((workspace_name, runtime)) => list_json(
                    &runtime,
                    &workspace,
                    &workspace_name,
                    generated_at,
                    state.operator_session(),
                    include_inactive_plugins,
                )?,
                Err(reason) => read_only_envelope(generated_at, Some(&workspace), &reason),
            })
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/auto-tasks/toggle` — set one definition's versioned `enabled` flag.
///
/// The observed profile of the shared toggle command decides; this handler
/// binds the operator, audits an attempted write and renders the outcome.
pub(super) async fn toggle_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskToggleRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_TOGGLE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.toggle",
                &body.name,
                "",
                &json!({"expected_enabled": body.expected_enabled, "enabled": body.enabled}),
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
    let outcome = match blocking("auto-task toggle", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        let profile = AutoTaskToggleProfile::Observed {
            expected_enabled: body.expected_enabled,
        };
        let enabled = body.enabled;
        move || Ok(runtime.auto_task_toggle_command(&name, enabled, profile))
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(response) => return *response,
    };
    let updated = match outcome {
        Ok(AutoTaskToggleOutcome::Written(updated)) => updated,
        Ok(AutoTaskToggleOutcome::AlreadySet(_)) => {
            return Json(json!({
                "name": body.name,
                "enabled": body.enabled,
                "changed": false,
                "message": if body.enabled { "Auto-task already enabled" } else { "Auto-task already disabled" },
            }))
            .into_response();
        }
        Ok(AutoTaskToggleOutcome::Stale { actual_enabled }) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "auto-task state changed while this action was pending; refresh before retrying",
                    "code": "stale_auto_task_state",
                    "actual_enabled": actual_enabled,
                })),
            )
                .into_response();
        }
        Ok(AutoTaskToggleOutcome::NotFound) => {
            return named_entity_not_found(
                "auto_task_not_found",
                format!("auto-task '{}' was not found", body.name),
            );
        }
        Err(AutoTaskToggleError::Read(error)) => return map_runtime_error(error),
        Err(AutoTaskToggleError::Write(error)) => {
            let error_message = error.to_string();
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.toggle",
                &body.name,
                "",
                &json!({"expected_enabled": body.expected_enabled, "enabled": body.enabled}),
                Some(&caller),
                None,
                Some(&error_message),
                started,
            )
            .await;
            return auto_task_control_error(error);
        }
    };
    record_operation_audit(
        &runtime,
        &workspace,
        "auto_task.toggle",
        &body.name,
        "",
        &json!({"expected_enabled": body.expected_enabled, "enabled": body.enabled}),
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    Json(json!({
        "name": updated.name,
        "enabled": updated.enabled,
        "changed": true,
        "message": if updated.enabled { "Auto-task enabled" } else { "Auto-task disabled" },
    }))
    .into_response()
}

/// `POST /api/auto-tasks/mint` — unconditional on-demand mint.
pub(super) async fn mint_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskMintRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    if !body.acknowledge_unconditional {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": UNCONDITIONAL_MINT_WARNING,
                "code": "unconditional_mint_not_acknowledged",
            })),
        )
            .into_response();
    }
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_MINT, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.mint",
                &body.name,
                "",
                &json!({"acknowledge_unconditional": body.acknowledge_unconditional}),
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
    let minted = match blocking("auto-task mint", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        move || Ok(runtime.auto_task_mint(&name))
    })
    .await
    {
        Ok(Ok(task)) => task,
        Ok(Err(error)) => {
            let error_message = error.to_string();
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.mint",
                &body.name,
                "",
                &json!({"acknowledge_unconditional": true}),
                Some(&caller),
                None,
                Some(&error_message),
                started,
            )
            .await;
            return auto_task_control_error(error);
        }
        Err(response) => return *response,
    };
    record_operation_audit(
        &runtime,
        &workspace,
        "auto_task.mint",
        &body.name,
        "",
        &json!({
            "acknowledge_unconditional": true,
            "task_id": minted.id.to_string(),
        }),
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    Json(json!({
        "name": body.name,
        "task_id": minted.id.to_string(),
        "status": minted.status,
        "message": format!("Minted {} ({})", minted.id, minted.status),
    }))
    .into_response()
}

/// `POST /api/auto-tasks/delete` — remove one definition, its scheduler cursor
/// and its delivery-consumer state.
///
/// An open minted task is a conflict the caller resolves with `force`. The
/// response names those tasks. A caller without operator authority is refused
/// and the denial is audited, as are a successful delete and a core refusal.
pub(super) async fn delete_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskDeleteRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    let reason = trimmed_delete_reason(body.reason.as_deref());
    let arguments = json!({"reason": reason, "force": body.force});
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_DELETE, state.operator_session()) {
        Ok(caller) => caller,
        Err(denial) => {
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.delete",
                &body.name,
                "",
                &arguments,
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
    let report = match blocking("auto-task delete", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        let reason = reason.clone();
        let force = body.force;
        move || {
            Ok(runtime.auto_task_delete(AutoTaskDeleteParams {
                name,
                reason,
                force,
            }))
        }
    })
    .await
    {
        Ok(Ok(report)) => report,
        Ok(Err(error)) => {
            let error_message = error.to_string();
            record_operation_audit(
                &runtime,
                &workspace,
                "auto_task.delete",
                &body.name,
                "",
                &arguments,
                Some(&caller),
                None,
                Some(&error_message),
                started,
            )
            .await;
            return delete_refusal(&body.name, error);
        }
        Err(response) => return *response,
    };
    record_operation_audit(
        &runtime,
        &workspace,
        "auto_task.delete",
        &report.name,
        "",
        &arguments,
        Some(&caller),
        None,
        None,
        started,
    )
    .await;
    match delete::project_auto_task_delete_receipt(&report, AutoTaskDeleteProfile::Http) {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

fn trimmed_delete_reason(reason: Option<&str>) -> Option<String> {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(ToOwned::to_owned)
}

/// Task ids from core's open-minted-task refusal. The sentence is the contract
/// `auto_task_delete` returns; the response carries the ids so the dashboard
/// does not scrape it.
fn open_minted_task_ids(message: &str) -> Option<Vec<String>> {
    const MARKER: &str = "still has open minted tasks: ";
    let start = message.find(MARKER)? + MARKER.len();
    let list = message[start..].split(';').next()?.trim();
    let ids = list
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    (!ids.is_empty()).then_some(ids)
}

fn delete_refusal(name: &str, error: orbit_core::OrbitError) -> Response {
    match error {
        orbit_core::OrbitError::InvalidInput(message) => {
            if let Some(open_tasks) = open_minted_task_ids(&message) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": message,
                        "code": "open_minted_tasks",
                        "open_tasks": open_tasks,
                    })),
                )
                    .into_response();
            }
            if message == format!("no such auto-task '{name}'") {
                return named_entity_not_found(
                    "auto_task_not_found",
                    format!("auto-task '{name}' was not found"),
                );
            }
            auto_task_control_error(orbit_core::OrbitError::InvalidInput(message))
        }
        other => auto_task_control_error(other),
    }
}

pub(super) fn auto_task_control_error(error: orbit_core::OrbitError) -> Response {
    match error {
        orbit_core::OrbitError::CapabilityRefused(message) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message, "code": "capability_refused"})),
        )
            .into_response(),
        orbit_core::OrbitError::PolicyDenied(message) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message, "code": "policy_denied"})),
        )
            .into_response(),
        other => map_runtime_error(other),
    }
}

pub(super) async fn resolve_workspace_blocking(
    state: &DashboardState,
    workspace: &str,
) -> Result<Arc<OrbitRuntime>, Box<Response>> {
    let state = state.clone();
    let workspace = workspace.to_string();
    match blocking("resolve workspace", {
        let workspace = workspace.clone();
        move || Ok(resolve_workspace(&state, &workspace))
    })
    .await
    {
        Ok(Ok((_, runtime))) => Ok(runtime),
        Ok(Err(reason)) => Err(Box::new(selection_conflict("workspace_mismatch", reason))),
        Err(response) => Err(response),
    }
}

pub(super) fn resolve_workspace(
    state: &DashboardState,
    workspace: &str,
) -> Result<(String, Arc<OrbitRuntime>), String> {
    let pinned = state.pin();
    let entry = pinned.entries().iter().find(|entry| entry.id == workspace);
    match entry {
        None => Err(format!(
            "workspace '{workspace}' is not a concrete active selection"
        )),
        Some(entry) if !entry.active => Err(format!(
            "workspace '{workspace}' is inactive; select an active workspace"
        )),
        Some(entry) => match pinned.runtime_for(workspace) {
            Ok(runtime) => Ok((entry.name.clone(), runtime)),
            Err(_) => Err(format!(
                "workspace '{workspace}' is not a concrete active selection"
            )),
        },
    }
}

fn read_only_envelope(generated_at: DateTime<Utc>, workspace: Option<&str>, reason: &str) -> Value {
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "workspace": workspace,
        "controls_authorized": false,
        "capabilities": {
            "auto_task_toggle": {"authorized": false, "reason": reason},
            "auto_task_mint": {"authorized": false, "reason": reason},
            "auto_task_schedule": {"authorized": false, "reason": reason},
            "auto_task_delete": {"authorized": false, "reason": reason},
        },
        "read_only_reason": reason,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "definitions": [],
        "inactive_plugin_count": 0,
        "cursor_state_error": null,
        "load_errors": [],
    })
}

/// `inactive_plugin_count` counts the definitions whose seeding plugin is off
/// in this workspace whether or not they are listed, so the panel can offer
/// to show them; they never enter the listed counts or next-mint summary.
fn list_json(
    runtime: &OrbitRuntime,
    workspace: &str,
    workspace_name: &str,
    generated_at: DateTime<Utc>,
    operator_session: bool,
    include_inactive_plugins: bool,
) -> Result<Value, orbit_core::OrbitError> {
    let now = Utc::now();
    let listing = runtime.auto_task_listing_state_lenient(include_inactive_plugins, now);
    // One freshness pass for the request, with no task-body hydration, that
    // reads only the tasks carrying a listed definition's tag: the rest of
    // the workspace never leaves the index [ORB-21824]. The metadata listing
    // is newest first, exactly as the per-tag listing was.
    let tags = listing
        .rows
        .iter()
        .map(|row| (auto_task_tag(&row.listed.definition.name), Vec::new()))
        .collect::<BTreeMap<_, Vec<&Task>>>();
    let tasks = if tags.is_empty() {
        Ok(Vec::new())
    } else {
        runtime.list_task_metadata_with_any_tag(&tags.keys().cloned().collect())
    };
    let mut instances = tags;
    for task in tasks.as_ref().map_or(&[][..], Vec::as_slice) {
        for tag in &task.tags {
            if let Some(group) = instances.get_mut(tag) {
                group.push(task);
            }
        }
    }
    let definitions = listing
        .rows
        .iter()
        .map(|row| {
            runtime.project_auto_task_listing_row(
                row,
                AutoTaskListingProfile::Http {
                    minted: tasks.as_ref().map(|_| {
                        instances
                            .get(&auto_task_tag(&row.listed.definition.name))
                            .map_or(&[][..], Vec::as_slice)
                    }),
                    now,
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let role_refusal = runtime
        .coordination_task_write_refusal()
        .map(|error| error.to_string());
    let toggle_capability = role_refusal.as_deref().map_or_else(
        || action_capability(&DASHBOARD_AUTO_TASK_TOGGLE, operator_session),
        |reason| json!({"authorized": false, "reason": reason}),
    );
    let mint_capability = role_refusal.as_deref().map_or_else(
        || action_capability(&DASHBOARD_AUTO_TASK_MINT, operator_session),
        |reason| json!({"authorized": false, "reason": reason}),
    );
    let schedule_capability = role_refusal.as_deref().map_or_else(
        || action_capability(&DASHBOARD_AUTO_TASK_SCHEDULE, operator_session),
        |reason| json!({"authorized": false, "reason": reason}),
    );
    let delete_capability = role_refusal.as_deref().map_or_else(
        || action_capability(&DASHBOARD_AUTO_TASK_DELETE, operator_session),
        |reason| json!({"authorized": false, "reason": reason}),
    );
    let controls_authorized = authorized_caller(&DASHBOARD_AUTO_TASK_TOGGLE, operator_session)
        .is_ok()
        && authorized_caller(&DASHBOARD_AUTO_TASK_MINT, operator_session).is_ok()
        && role_refusal.is_none();
    Ok(json!({
        "generated_at": generated_at.to_rfc3339(),
        "workspace": workspace,
        "workspace_name": workspace_name,
        "controls_authorized": controls_authorized,
        "capabilities": {
            "auto_task_toggle": toggle_capability,
            "auto_task_mint": mint_capability,
            "auto_task_schedule": schedule_capability,
            "auto_task_delete": delete_capability,
        },
        "read_only_reason": null,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "cron_zone": host_cron_zone(),
        "definitions": definitions,
        "inactive_plugin_count": listing.inactive_plugin_count,
        "cursor_state_error": listing.cursor_state_error,
        "load_errors": listing.load_errors.iter().map(|error| json!({
            "path": error.path.as_ref().map(|path| path.display().to_string()),
            "message": error.message,
        })).collect::<Vec<_>>(),
    }))
}
