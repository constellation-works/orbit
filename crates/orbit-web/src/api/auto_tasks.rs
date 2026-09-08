//! Auto-task inspection, toggle, and manual mint for the dashboard [ORB-10876].

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_TASK_MINT, DASHBOARD_AUTO_TASK_TOGGLE,
};
use orbit_core::OrbitRuntime;
use orbit_core::application::auto_tasks::schedule::next_scheduled_slot;
use orbit_core::application::auto_tasks::{
    AutoTaskCursor, collect_auto_tasks, cursor_state_path, load_cursor_state,
};
use orbit_core::application::routines::ScheduleDisplayState;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, auto_task_tag,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::blocking;
use super::map_runtime_error;
use super::routines::{
    OperationsQuery, action_capability, authorization_denied, authorized_caller,
    explicit_workspace, named_entity_not_found, next_evaluation_json, record_operation_audit,
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
    let name = body.name.clone();
    let current = match blocking("auto-task show", {
        let runtime = runtime.clone();
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
    let updated = match blocking("auto-task toggle", {
        let runtime = runtime.clone();
        let name = body.name.clone();
        let enabled = body.enabled;
        move || Ok(runtime.auto_task_toggle(&name, enabled))
    })
    .await
    {
        Ok(Ok(updated)) => updated,
        Ok(Err(error)) => {
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
        Err(response) => return *response,
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
            );
            return map_runtime_error(error);
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
        "capabilities": {
            "auto_task_toggle": {"authorized": false, "reason": reason},
            "auto_task_mint": {"authorized": false, "reason": reason},
        },
        "read_only_reason": reason,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "definitions": [],
        "cursor_state_error": null,
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
    let cursor_load = load_cursor_state(&cursor_state_path(&runtime.paths().state_dir));
    let cursor_state_error = cursor_load.as_ref().err().map(ToString::to_string);
    let cursors = cursor_load.as_ref().ok();
    let now = Utc::now();
    let definitions = collection
        .definitions
        .iter()
        .map(|loaded| {
            definition_json(
                runtime,
                &loaded.definition,
                cursors.and_then(|state| state.definitions.get(&loaded.definition.name)),
                cursor_state_error.is_some(),
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
        "capabilities": {
            "auto_task_toggle": action_capability(&DASHBOARD_AUTO_TASK_TOGGLE),
            "auto_task_mint": action_capability(&DASHBOARD_AUTO_TASK_MINT),
        },
        "read_only_reason": null,
        "unconditional_mint_warning": UNCONDITIONAL_MINT_WARNING,
        "definitions": definitions,
        "cursor_state_error": cursor_state_error,
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
    cursor_state_unavailable: bool,
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
            "pending": cursor.pending.as_ref().map(|pending| json!({
                "slot": pending.slot,
                "task_id": pending.task_id,
            })),
        })),
        "last_minted_task_id": last_minted_task_id,
        "last_minted_task_status": last_minted_task_status,
        "next_evaluation": next_evaluation_projection(
            definition.enabled,
            &definition.schedule,
            cursor,
            cursor_state_unavailable,
            automation.as_ref(),
            now,
        ),
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
            "{} deliveries on {} ({})",
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

fn next_evaluation_projection(
    enabled: bool,
    schedule: &AutoTaskSchedule,
    cursor: Option<&AutoTaskCursor>,
    cursor_state_unavailable: bool,
    automation: Option<&Value>,
    now: DateTime<Utc>,
) -> Value {
    if !enabled {
        return next_evaluation_json(
            ScheduleDisplayState::Disabled,
            projected_next_slot(schedule, cursor, now),
        );
    }
    if cursor_state_unavailable || automation_unavailable(automation) {
        return next_evaluation_json(ScheduleDisplayState::Unavailable, None);
    }
    match schedule {
        AutoTaskSchedule::Deliveries { .. } => {
            next_evaluation_json(ScheduleDisplayState::Waiting, None)
        }
        AutoTaskSchedule::Cron { .. } | AutoTaskSchedule::Interval { .. } => {
            if cursor.is_none() {
                return next_evaluation_json(ScheduleDisplayState::NeverObserved, None);
            }
            match projected_next_slot(schedule, cursor, now) {
                Some(at) => next_evaluation_json(ScheduleDisplayState::Scheduled, Some(at)),
                None => next_evaluation_json(ScheduleDisplayState::Unavailable, None),
            }
        }
    }
}

fn automation_unavailable(automation: Option<&Value>) -> bool {
    automation
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
        .is_some_and(|reason| reason == "source_unavailable" || reason == "state_unavailable")
}

/// The schedule's next occurrence, rendered for the dashboard.
///
/// This is the next scheduled arrival, never catch-up eligibility: a definition
/// with a missed slot pending is due for that earlier slot while this points
/// forward. The arithmetic belongs to the auto-task scheduler, so this only
/// adapts the cursor and renders the result; a schedule that cannot be
/// projected (unparseable cron, interval with no anchor) yields `None` and the
/// caller labels the row.
fn projected_next_slot(
    schedule: &AutoTaskSchedule,
    cursor: Option<&AutoTaskCursor>,
    now: DateTime<Utc>,
) -> Option<String> {
    let baseline = cursor.and_then(cursor_baseline);

    next_scheduled_slot(schedule, baseline, now)
        .ok()
        .flatten()
        .map(|slot| slot.to_rfc3339())
}

/// The cursor's first-observed slot, which anchors interval projections.
fn cursor_baseline(cursor: &AutoTaskCursor) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&cursor.baseline_at)
        .ok()
        .map(|baseline| baseline.with_timezone(&Utc))
}
