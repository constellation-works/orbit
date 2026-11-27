//! Auto-task inspection and definition controls for the dashboard.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_CREATE, DASHBOARD_AUTO_TASK_DELETE, DASHBOARD_AUTO_TASK_MINT,
    DASHBOARD_AUTO_TASK_SCHEDULE, DASHBOARD_AUTO_TASK_TOGGLE,
};
use orbit_core::Core;
use orbit_core::app::DashboardOrigin;
use orbit_core::app::auto_task::{
    AutoTaskDeleteError, AutoTaskDeleteParams, AutoTaskDeleteProfile, AutoTaskDeleteRefusal,
    AutoTaskToggleError, AutoTaskToggleOutcome, Command, DashboardWrite, DashboardWritten, Listing,
    Outcome, exec, project_auto_task_delete_receipt,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::map_runtime_error;
use super::routines::{
    OperationsQuery, action_capability, authorized_caller, dashboard_caller, explicit_workspace,
    host_cron_zone, named_entity_not_found, selection_conflict, unexpected_outcome,
};
use super::{bad_request, blocking};
use crate::state::DashboardState;

const UNCONDITIONAL_MINT_WARNING: &str = "Manual mint ignores this definition's schedule, \
enabled flag, and scheduler dedupe policy. It does not read or write the host-local cursor.";

#[derive(Debug, Deserialize)]
pub(super) struct AutoTaskCreateRequest {
    enabled: Option<bool>,
    #[serde(flatten)]
    fields: Map<String, Value>,
}

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
            let (workspace_name, core) = match resolve_workspace(&state, &workspace) {
                Ok(resolved) => resolved,
                Err(reason) => {
                    return Ok(read_only_envelope(generated_at, Some(&workspace), &reason));
                }
            };
            let listing = match exec(
                &core,
                Command::Listing {
                    include_inactive_plugins,
                },
            )? {
                Outcome::Listing(listing) => listing,
                Outcome::Value(_) | Outcome::Dashboard(_) => {
                    return Err(OrbitError::Execution(
                        "auto-task list answered with another command's outcome".to_string(),
                    ));
                }
            };
            Ok(list_json(
                *listing,
                &workspace,
                &workspace_name,
                generated_at,
                state.operator_session(),
                state.env(),
            ))
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/auto-tasks/create` — create a workspace definition using the
/// structured fields accepted by auto-task add.
pub(super) async fn create_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskCreateRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let core = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(core) => core,
        Err(response) => return *response,
    };
    let written = auto_task_write(
        "auto-task create",
        core,
        dashboard_caller(&state, Some(&workspace), ""),
        DashboardWrite::Create {
            fields: Value::Object(body.fields),
            enabled: body.enabled,
        },
    )
    .await;
    let outcome = match written {
        Ok(DashboardWritten::Created(outcome)) => outcome,
        Ok(_) => return unexpected_outcome("auto-task create"),
        Err(response) => return *response,
    };
    match outcome {
        Ok(definition) => (StatusCode::CREATED, Json(definition)).into_response(),
        Err(error) => {
            let message = error.to_string();
            match error {
                OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } => {
                    bad_request(message)
                }
                OrbitError::CapabilityRefused(_) => (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": message, "code": "capability_refused"})),
                )
                    .into_response(),
                OrbitError::PolicyDenied(_) => (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": message, "code": "policy_denied"})),
                )
                    .into_response(),
                other => auto_task_control_error(other),
            }
        }
    }
}

/// `POST /api/auto-tasks/toggle` — set one definition's versioned `enabled` flag.
///
/// The auto-task app binds the operator, decides with the observed profile
/// of the shared toggle command and audits an attempted write; this handler
/// renders the outcome.
pub(super) async fn toggle_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskToggleRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let core = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(core) => core,
        Err(response) => return *response,
    };
    let written = auto_task_write(
        "auto-task toggle",
        core,
        dashboard_caller(&state, Some(&workspace), ""),
        DashboardWrite::Toggle {
            name: body.name.clone(),
            expected_enabled: body.expected_enabled,
            enabled: body.enabled,
        },
    )
    .await;
    let outcome = match written {
        Ok(DashboardWritten::Toggled(outcome)) => outcome,
        Ok(_) => return unexpected_outcome("auto-task toggle"),
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
        Err(AutoTaskToggleError::Write(error)) => return auto_task_control_error(error),
    };
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
    let core = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(core) => core,
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
    let written = auto_task_write(
        "auto-task mint",
        core,
        dashboard_caller(&state, Some(&workspace), ""),
        DashboardWrite::Mint {
            name: body.name.clone(),
        },
    )
    .await;
    let minted = match written {
        Ok(DashboardWritten::Minted(Ok(task))) => task,
        Ok(DashboardWritten::Minted(Err(error))) => return auto_task_control_error(error),
        Ok(_) => return unexpected_outcome("auto-task mint"),
        Err(response) => return *response,
    };
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
    let core = match resolve_workspace_blocking(&state, &workspace).await {
        Ok(core) => core,
        Err(response) => return *response,
    };
    let written = auto_task_write(
        "auto-task delete",
        core,
        dashboard_caller(&state, Some(&workspace), ""),
        DashboardWrite::Delete(AutoTaskDeleteParams {
            name: body.name.clone(),
            reason: trimmed_delete_reason(body.reason.as_deref()),
            force: body.force,
        }),
    )
    .await;
    let report = match written {
        Ok(DashboardWritten::Deleted(Ok(report))) => report,
        Ok(DashboardWritten::Deleted(Err(error))) => return delete_refusal(&body.name, error),
        Ok(_) => return unexpected_outcome("auto-task delete"),
        Err(response) => return *response,
    };
    match project_auto_task_delete_receipt(&report, AutoTaskDeleteProfile::Http) {
        Ok(receipt) => Json(receipt).into_response(),
        Err(error) => map_runtime_error(error),
    }
}

/// Run one governed auto-task write on the blocking pool and return what the
/// admitted write did, or the response that already answers the request.
pub(super) async fn auto_task_write(
    label: &'static str,
    core: Arc<Core>,
    caller: DashboardOrigin,
    write: DashboardWrite,
) -> Result<DashboardWritten, Box<Response>> {
    match blocking(label, move || {
        exec(&core, Command::Dashboard(caller, write))
    })
    .await?
    {
        Outcome::Dashboard(written) => Ok(*written),
        Outcome::Value(_) | Outcome::Listing(_) => Err(Box::new(unexpected_outcome(label))),
    }
}

fn trimmed_delete_reason(reason: Option<&str>) -> Option<String> {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(ToOwned::to_owned)
}

fn delete_refusal(name: &str, error: AutoTaskDeleteError) -> Response {
    match error {
        AutoTaskDeleteError::Refusal(refusal) => {
            let message = refusal.message();
            match refusal {
                AutoTaskDeleteRefusal::OpenMintedTasks { open_tasks, .. } => (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": message,
                        "code": "open_minted_tasks",
                        "open_tasks": open_tasks,
                    })),
                )
                    .into_response(),
                AutoTaskDeleteRefusal::NotFound { .. } => named_entity_not_found(
                    "auto_task_not_found",
                    format!("auto-task '{name}' was not found"),
                ),
            }
        }
        AutoTaskDeleteError::Other(error) => auto_task_control_error(error),
    }
}

pub(super) fn auto_task_control_error(error: orbit_common::OrbitError) -> Response {
    match error {
        orbit_common::OrbitError::CapabilityRefused(message) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": message, "code": "capability_refused"})),
        )
            .into_response(),
        orbit_common::OrbitError::PolicyDenied(message) => (
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
) -> Result<Arc<Core>, Box<Response>> {
    let state = state.clone();
    let workspace = workspace.to_string();
    match blocking("resolve workspace", {
        let workspace = workspace.clone();
        move || Ok(resolve_workspace(&state, &workspace))
    })
    .await
    {
        Ok(Ok((_, core))) => Ok(core),
        Ok(Err(reason)) => Err(Box::new(selection_conflict("workspace_mismatch", reason))),
        Err(response) => Err(response),
    }
}

/// The workspace's name and its app entry, when `workspace` selects one
/// concrete active workspace.
pub(super) fn resolve_workspace(
    state: &DashboardState,
    workspace: &str,
) -> Result<(String, Arc<Core>), String> {
    let pinned = state.pin();
    let entry = pinned.entries().iter().find(|entry| entry.id == workspace);
    match entry {
        None => Err(format!(
            "workspace '{workspace}' is not a concrete active selection"
        )),
        Some(entry) if !entry.active => Err(format!(
            "workspace '{workspace}' is inactive; select an active workspace"
        )),
        Some(entry) => match pinned.core_for(workspace) {
            Ok(core) => Ok((entry.name.clone(), core)),
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
            "auto_task_create": {"authorized": false, "reason": reason},
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
/// The auto-task app reads the listing; this renders it beside what the
/// session may do.
fn list_json(
    listing: Listing,
    workspace: &str,
    workspace_name: &str,
    generated_at: DateTime<Utc>,
    operator_session: bool,
    env: &orbit_common::Env,
) -> Value {
    let role_refusal = listing.role_refusal;
    let create_capability = role_refusal.as_deref().map_or_else(
        || action_capability(&DASHBOARD_AUTO_TASK_CREATE, operator_session),
        |reason| json!({"authorized": false, "reason": reason}),
    );
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
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "workspace": workspace,
        "workspace_name": workspace_name,
        "controls_authorized": controls_authorized,
        "capabilities": {
            "auto_task_create": create_capability,
            "auto_task_toggle": toggle_capability,
            "auto_task_mint": mint_capability,
            "auto_task_schedule": schedule_capability,
            "auto_task_delete": delete_capability,
        },
        "read_only_reason": null,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "cron_zone": host_cron_zone(env),
        "definitions": listing.definitions,
        "inactive_plugin_count": listing.inactive_plugin_count,
        "cursor_state_error": listing.cursor_state_error,
        "load_errors": listing.load_errors.iter().map(|error| json!({
            "path": error.path.as_ref().map(|path| path.display().to_string()),
            "message": error.message,
        })).collect::<Vec<_>>(),
    })
}
