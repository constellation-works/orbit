use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, DESCRIPTION_DOCUMENT, TASK_FIELDS_UPDATED_EVENT, Task,
    TaskComment, TaskStatus, task_fields_updated_documents,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

use super::super::persist;
use super::exclusion;
use crate::OrbitRuntime;

/// Upper bound on `recheck_max_age_hours`: one year.
const HARD_MAX_RECHECK_AGE_HOURS: u64 = 24 * 365;

/// The run's re-check request: `full_pilot` forces a full assessment, and
/// `recheck_max_age_hours` bounds a prior assessment's age (0 disables
/// re-checks).
pub(super) fn recheck_request(
    action: &str,
    input: &Value,
) -> Result<super::super::recheck::RecheckRequest, DispatchError> {
    let full = match input.get("full_pilot") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(full)) => *full,
        Some(_) => {
            return Err(DispatchError::action_failed(
                action,
                "`full_pilot` must be a boolean",
            ));
        }
    };
    let hours = match input.get("recheck_max_age_hours") {
        None | Some(Value::Null) => super::super::recheck::DEFAULT_RECHECK_MAX_AGE_HOURS,
        Some(value) => value
            .as_u64()
            .filter(|hours| *hours <= HARD_MAX_RECHECK_AGE_HOURS)
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "`recheck_max_age_hours` must be between 0 and {HARD_MAX_RECHECK_AGE_HOURS}"
                    ),
                )
            })?,
    };
    Ok(super::super::recheck::RecheckRequest {
        full,
        max_age: chrono::Duration::hours(hours as i64),
        now: chrono::Utc::now(),
    })
}

/// The pilot run this preparation belongs to. The dispatcher names it
/// `run_id`, or `job_run_id` when the input already carried a `run_id` token.
pub(super) fn pilot_run_id(input: &Value) -> Option<&str> {
    ["job_run_id", "run_id"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|run_id| !run_id.is_empty())
}

/// The superseded outcome for an automatically selected task that another
/// writer moved out of automatic selection after its envelope was read, or
/// `None` while hydration still selects it.
pub(super) fn left_automatic_selection(
    selected: &Value,
    task: &Task,
    stale_repilot: bool,
    pr_route_lacks_forge: bool,
) -> Option<Value> {
    let exclusion = exclusion::automatic_exclusion_reason(
        task.status,
        &task.context_files,
        task.complexity,
        &task.tags,
    )?;
    // A stale-repilot selection keeps its assessed selectors and complexity:
    // they are the stale assessment's output, not a move out of selection.
    // Independent exclusions hidden behind that reason (no-diff, missing
    // forge) still mark a move out of selection; active preparations are
    // re-checked at reservation below.
    if stale_repilot
        && exclusion == "context_files_not_empty"
        && exclusion::no_diff_exclusion_reason(&task.tags).is_none()
        && !pr_route_lacks_forge
    {
        return None;
    }
    let (reason, detail) = if task.status.is_terminal() {
        ("task_terminal", "task became terminal after selection")
    } else if selected["status"] != json!(task.status) {
        (
            "status_changed",
            "a durable task transition superseded preparation",
        )
    } else {
        ("task_edited", "task fields changed after selection")
    };
    Some(super::super::superseded_outcome(
        &task.id,
        reason,
        task.status,
        detail,
    ))
}

/// Whether `task_id`'s latest applied assessment no longer certifies its
/// current meaning (description or acceptance criteria changed after it, or
/// `context_files` changed after it while the assessment holds a warning),
/// no live execution claim holds the task, and no failed repilot holds its
/// current requirements:
/// automatic discovery reselects such a task for a fresh pilot instead of
/// leaving it held for an operator repilot. A failed repilot is the newest
/// `task_pilot_unresolved` record after the latest apply; it keeps the task
/// held until a later description/criteria edit, a newer assessment or an
/// operator action, matching the drain's unresolved hold.
/// `None` when the task's pilot assessment cannot be resolved.
pub(super) fn stale_repilot_candidate(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
    claimed_tasks: &BTreeSet<String>,
) -> Result<Option<bool>, DispatchError> {
    if claimed_tasks.contains(task_id) {
        return Ok(Some(false));
    }
    let task = runtime.get_task(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id}: {error}"))
    })?;
    if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(Some(false));
    }
    let history = runtime.get_task_history(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} history: {error}"))
    })?;
    let latest_applied = history
        .iter()
        .rev()
        .find(|entry| entry.event == "task_pilot_applied")
        .map(|entry| entry.at);
    // Only a later requirements or `context_files` edit releases a failed
    // repilot. Envelope, plan and execution-summary updates must not restart
    // automatic retries.
    let latest_requirements_edit = history
        .iter()
        .rev()
        .find(|entry| {
            entry.event == super::super::freshness::PILOT_CONTEXT_FILES_EDITED_EVENT
                || (entry.event == TASK_FIELDS_UPDATED_EVENT
                    && task_fields_updated_documents(entry.note.as_deref().unwrap_or_default())
                        .iter()
                        .any(|document| {
                            matches!(
                                *document,
                                DESCRIPTION_DOCUMENT | ACCEPTANCE_CRITERIA_DOCUMENT
                            )
                        }))
        })
        .map(|entry| entry.at);
    if history.iter().any(|entry| {
        entry.event == "task_pilot_unresolved"
            && latest_applied.is_none_or(|applied| entry.at > applied)
            && latest_requirements_edit.is_none_or(|edited| entry.at >= edited)
    }) {
        return Ok(Some(false));
    }
    let Some(comments) = resolved_pilot_comments(runtime, action, task_id)? else {
        return Ok(None);
    };
    let Some((_, receipt)) = super::super::pilot_receipts(&comments).next() else {
        return Ok(Some(false));
    };
    if !super::super::is_operation_id(receipt.operation_id) {
        return Ok(Some(false));
    }
    let Ok(audit) = serde_json::from_str::<Value>(receipt.body) else {
        return Ok(Some(false));
    };
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    if super::super::freshness::pilot_requirements_stale(
        &task,
        &audit,
        receipt.operation_id,
        &|| Ok(history.clone()),
    )
    .map_err(failed)?
    {
        return Ok(Some(true));
    }
    // A `context_files` edit answers a holding warning or admission hold: the
    // usual fix for a scope or footprint finding. A clean assessment is not
    // reopened by one.
    if !super::super::freshness::pilot_context_files_edited(&history, receipt.operation_id) {
        return Ok(Some(false));
    }
    let warned = audit.get("assessment").is_some_and(|assessment| {
        !super::super::promotion::warning_hold_findings(assessment, &task.dependencies(), &|id| {
            runtime.warning_prerequisite_is_done(id)
        })
        .is_empty()
    });
    if warned {
        return Ok(Some(true));
    }
    runtime
        .pilot_admission_hold(task_id)
        .map(|hold| Some(hold.is_some()))
        .map_err(failed)
}

/// Whether `task_id` already carries an applied assessment; `None` when its
/// pilot assessment cannot be resolved.
pub(super) fn already_piloted(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
) -> Result<Option<bool>, DispatchError> {
    let Some(comments) = resolved_pilot_comments(runtime, action, task_id)? else {
        return Ok(None);
    };
    if runtime
        .task_pilot_scheduling_assessment(task_id, &comments)
        .map_err(|error| {
            DispatchError::action_failed(action, format!("read task {task_id} assessment: {error}"))
        })?
        .is_some()
    {
        return Ok(Some(true));
    }
    let history = runtime.get_task_history(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} history: {error}"))
    })?;
    let Some(applied) = history
        .iter()
        .rev()
        .find(|entry| entry.event == "task_pilot_applied")
    else {
        return Ok(Some(false));
    };
    let no_target_assessment = persist::no_target_pilot_assessment(applied, &comments);
    Ok(Some(no_target_assessment.is_some()))
}

/// Discovery exclusion for a task whose pilot assessment artifact is missing,
/// oversized or fails its digest check. It holds that task alone.
pub(super) const PILOT_ASSESSMENT_UNAVAILABLE: &str = "pilot_assessment_unavailable";

/// `task_id`'s comments with pilot audits resolved, or `None` when an
/// assessment artifact cannot be resolved. A failed comment read still fails
/// the pass.
fn resolved_pilot_comments(
    runtime: &OrbitRuntime,
    action: &str,
    task_id: &str,
) -> Result<Option<Vec<TaskComment>>, DispatchError> {
    let comments = runtime.get_task_comments(task_id).map_err(|error| {
        DispatchError::action_failed(action, format!("read task {task_id} comments: {error}"))
    })?;
    match runtime.resolve_task_pilot_comments(task_id, &comments) {
        Ok(resolved) => Ok(Some(resolved)),
        Err(error) => {
            tracing::warn!(
                target: "orbit_core::application::task::pilot::prepare",
                task_id,
                %error,
                "pilot assessment unavailable; excluded from discovery"
            );
            Ok(None)
        }
    }
}
