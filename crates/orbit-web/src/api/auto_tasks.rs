//! Auto-task inspection, toggle, and manual mint for the dashboard [ORB-10876].

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_TOGGLE, OPERATOR_OVERRIDE_ENV,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::auto_tasks::{
    collect_auto_tasks, cursor_state_path, load_cursor_state,
};
use orbit_core::application::routines::parse_cron;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, auto_task_tag,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::map_runtime_error;
use super::routines::{
    OperationsQuery, authorization_denied, authorized_caller, explicit_workspace,
    named_entity_not_found, record_operation_audit, selection_conflict,
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
    match resolve_workspace(&state, workspace) {
        Ok((workspace_name, runtime)) => Json(list_json(
            &runtime,
            workspace,
            &workspace_name,
            generated_at,
        ))
        .into_response(),
        Err(reason) => {
            Json(read_only_envelope(generated_at, Some(workspace), &reason)).into_response()
        }
    }
}

/// `POST /api/auto-tasks/toggle` — flip one definition's versioned `enabled` field.
pub(super) async fn toggle_auto_task(
    State(state): State<DashboardState>,
    Query(query): Query<OperationsQuery>,
    Json(body): Json<AutoTaskToggleRequest>,
) -> Response {
    let workspace = match explicit_workspace(&query) {
        Ok(workspace) => workspace.to_string(),
        Err(rejection) => return rejection.into_response(),
    };
    let runtime = match resolve_workspace(&state, &workspace) {
        Ok((_, runtime)) => runtime,
        Err(reason) => {
            return selection_conflict("workspace_mismatch", reason);
        }
    };
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_TOGGLE) {
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
            );
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let current = match runtime.auto_task_show(&body.name) {
        Ok(Some(definition)) => definition,
        Ok(None) => {
            return named_entity_not_found(
                "auto_task_not_found",
                format!("auto-task '{}' was not found", body.name),
            );
        }
        Err(error) => return map_runtime_error(error),
    };
    if current.enabled != body.expected_enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "auto-task state changed while this action was pending; refresh before retrying",
                "code": "stale_auto_task_state",
                "actual_enabled": current.enabled,
            })),
        )
            .into_response();
    }
    if current.enabled == body.enabled {
        return Json(json!({
            "name": body.name,
            "enabled": body.enabled,
            "changed": false,
            "message": if body.enabled { "Auto-task already enabled" } else { "Auto-task already disabled" },
        }))
        .into_response();
    }
    let updated = match runtime.auto_task_toggle(&body.name, body.enabled) {
        Ok(updated) => updated,
        Err(error) => {
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
            );
            return map_runtime_error(error);
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
    );
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
    let runtime = match resolve_workspace(&state, &workspace) {
        Ok((_, runtime)) => runtime,
        Err(reason) => {
            return selection_conflict("workspace_mismatch", reason);
        }
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
    let caller = match authorized_caller(&DASHBOARD_AUTO_TASK_MINT) {
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
            );
            return authorization_denied(denial);
        }
    };
    let started = Instant::now();
    let minted = match runtime.auto_task_mint(&body.name) {
        Ok(task) => task,
        Err(error) => {
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
            );
            return map_runtime_error(error);
        }
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
    );
    Json(json!({
        "name": body.name,
        "task_id": minted.id.to_string(),
        "status": minted.status,
        "message": format!("Minted {} ({})", minted.id, minted.status),
    }))
    .into_response()
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
        "read_only_reason": reason,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "definitions": [],
        "load_errors": [],
    })
}

fn list_json(
    runtime: &OrbitRuntime,
    workspace: &str,
    workspace_name: &str,
    generated_at: DateTime<Utc>,
) -> Value {
    let collection = collect_auto_tasks(&runtime.paths().local_dir);
    let cursors = load_cursor_state(&cursor_state_path(&runtime.paths().state_dir));
    let now = Utc::now();
    let definitions = collection
        .definitions
        .iter()
        .map(|loaded| {
            definition_json(
                runtime,
                &loaded.definition,
                cursors.definitions.get(&loaded.definition.name),
                now,
            )
        })
        .collect::<Vec<_>>();
    let controls_authorized = authorized_caller(&DASHBOARD_AUTO_TASK_TOGGLE).is_ok()
        && authorized_caller(&DASHBOARD_AUTO_TASK_MINT).is_ok();
    json!({
        "generated_at": generated_at.to_rfc3339(),
        "workspace": workspace,
        "workspace_name": workspace_name,
        "controls_authorized": controls_authorized,
        "read_only_reason": if controls_authorized {
            Value::Null
        } else {
            Value::String(format!(
                "Controls require an authorized operator session (set {OPERATOR_OVERRIDE_ENV} or use an interactive operator terminal)."
            ))
        },
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "definitions": definitions,
        "load_errors": collection.errors.iter().map(|error| json!({
            "path": error.path.as_ref().map(|path| path.display().to_string()),
            "message": error.message,
        })).collect::<Vec<_>>(),
    })
}

fn definition_json(
    runtime: &OrbitRuntime,
    definition: &AutoTaskDefinition,
    cursor: Option<&orbit_core::application::auto_tasks::AutoTaskCursor>,
    now: DateTime<Utc>,
) -> Value {
    let automation = match &definition.schedule {
        AutoTaskSchedule::Deliveries { .. } => Some(
            match orbit_core::application::automation::inspect_auto_task(runtime, definition, now) {
                Ok(diagnostic) => json!(diagnostic),
                Err(error) => json!({"reason":"state_unavailable","error":error.to_string()}),
            },
        ),
        _ => None,
    };
    let minted = tagged_instances(runtime, &definition.name);
    let open_duplicate = minted
        .as_ref()
        .is_ok_and(|tasks| tasks.iter().any(|task| is_open_status(task.status)));
    let last_minted = minted.as_ref().ok().and_then(|tasks| tasks.first());
    let last_minted_task_id = last_minted
        .map(|task| task.id.to_string())
        .or_else(|| cursor.and_then(|cursor| cursor.last_task_id.clone()));
    let last_minted_task_status = last_minted.map(|task| task.status);
    json!({
        "name": definition.name,
        "description": definition.description,
        "enabled": definition.enabled,
        "schedule": definition.schedule,
        "schedule_summary": schedule_summary(&definition.schedule),
        "automation": automation,
        "template_summary": template_summary(&definition.template),
        "template": {
            "title": definition.template.title,
            "crew": definition.template.crew,
            "status": definition.template.status,
            "priority": definition.template.priority,
            "required_tools": definition.template.required_tools,
        },
        "dedupe": match definition.dedupe {
            DedupePolicy::SkipIfOpen => "skip_if_open",
            DedupePolicy::Always => "always",
        },
        "last_evaluation": cursor.map(|cursor| json!({
            "kind": if cursor.last_fired_at.is_some() { "fired" } else { "baselined" },
            "baseline_at": cursor.baseline_at,
            "last_slot": cursor.last_slot,
            "last_fired_at": cursor.last_fired_at,
            "last_task_id": cursor.last_task_id,
        })),
        "last_minted_task_id": last_minted_task_id,
        "last_minted_task_status": last_minted_task_status,
        "next_evaluation": next_evaluation(&definition.schedule, cursor, now),
        "open_duplicate": open_duplicate,
        "may_create_open_duplicate": open_duplicate,
    })
}

fn tagged_instances(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<Vec<orbit_core::Task>, orbit_core::OrbitError> {
    let tag = auto_task_tag(name);
    runtime.list_tasks_by_tags(std::slice::from_ref(&tag))
}

fn is_open_status(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    )
}

fn schedule_summary(schedule: &AutoTaskSchedule) -> String {
    match schedule {
        AutoTaskSchedule::Deliveries {
            deliveries_landed: t,
        } => format!(
            "{} deliveries on {} ({:?})",
            t.threshold, t.branch, t.coverage
        ),
        AutoTaskSchedule::Cron { cron } => format!("cron {cron}"),
        AutoTaskSchedule::Interval { every_minutes } if *every_minutes == 1 => {
            "every 1 minute".to_string()
        }
        AutoTaskSchedule::Interval { every_minutes } => {
            format!("every {every_minutes} minutes")
        }
    }
}

fn template_summary(template: &AutoTaskTemplate) -> String {
    let title = if template.title.starts_with("[auto-task] ") {
        template.title.clone()
    } else {
        format!("[auto-task] {}", template.title)
    };
    let mut parts = vec![title];
    if let Some(crew) = template.crew.as_deref() {
        parts.push(format!("crew {crew}"));
    }
    parts.push(format!("status {}", template.status));
    parts.push(format!("priority {}", template.priority));
    parts.join(" · ")
}

fn next_evaluation(
    schedule: &AutoTaskSchedule,
    cursor: Option<&orbit_core::application::auto_tasks::AutoTaskCursor>,
    now: DateTime<Utc>,
) -> Option<String> {
    let cursor = cursor?;
    match schedule {
        AutoTaskSchedule::Deliveries { .. } => None,
        AutoTaskSchedule::Cron { cron } => {
            let parsed = parse_cron(cron).ok()?;
            let now_local = now.with_timezone(&Local);
            parsed
                .find_next_occurrence(&now_local, false)
                .ok()
                .map(|slot| slot.with_timezone(&Utc).to_rfc3339())
        }
        AutoTaskSchedule::Interval { every_minutes } => {
            next_interval(*every_minutes, cursor, now).map(|slot| slot.to_rfc3339())
        }
    }
}

fn next_interval(
    every_minutes: u64,
    cursor: &orbit_core::application::auto_tasks::AutoTaskCursor,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if every_minutes == 0 {
        return None;
    }
    let baseline = DateTime::parse_from_rfc3339(&cursor.baseline_at)
        .ok()?
        .with_timezone(&Utc);
    let last_slot = cursor
        .last_slot
        .as_deref()
        .and_then(|slot| DateTime::parse_from_rfc3339(slot).ok())
        .map(|slot| slot.with_timezone(&Utc));
    let interval = Duration::minutes(i64::try_from(every_minutes).ok()?);
    let floor = last_slot.unwrap_or(baseline);
    if now < baseline {
        return Some(baseline + interval);
    }
    let elapsed = now.signed_duration_since(baseline).num_minutes();
    let period = i64::try_from(every_minutes).ok()?;
    let mut periods = elapsed / period;
    let mut next = baseline + Duration::minutes(period * periods);
    if next <= floor || next <= now {
        periods += 1;
        next = baseline + Duration::minutes(period * periods);
    }
    if next <= now {
        next += interval;
    }
    Some(next)
}
