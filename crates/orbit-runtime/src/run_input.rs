use std::collections::BTreeSet;

use serde_json::Value;

/// Environment marker emitted with the owning run ID for a managed activity.
pub const ORBIT_MANAGED_RUN_CONTEXT_ENV: &str = "ORBIT_MANAGED_RUN_CONTEXT";

/// Read the managed run ID declared by this process for routing and attribution.
/// Children can change their environment, so this ID cannot authenticate a resident.
pub fn managed_run_context_run_id_from_env() -> Option<String> {
    let managed = std::env::var(ORBIT_MANAGED_RUN_CONTEXT_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    if !managed {
        return None;
    }

    std::env::var("ORBIT_RUN_ID")
        .ok()
        .and_then(|value| non_empty(&value).map(ToOwned::to_owned))
}

/// The task a managed run's environment names (`ORBIT_TASK_ID`), honored
/// only with the managed marker and a non-blank run id, like
/// [`managed_run_context_run_id_from_env`].
pub fn managed_run_context_task_id_from_env() -> Option<String> {
    managed_run_context_run_id_from_env()?;
    std::env::var("ORBIT_TASK_ID")
        .ok()
        .and_then(|value| non_empty(&value).map(ToOwned::to_owned))
}

/// Whether this process declares a managed run context.
pub fn managed_run_context_from_env() -> bool {
    managed_run_context_run_id_from_env().is_some()
}

/// Managed provenance sufficient to route a child to its dispatching
/// workspace and registry. This does not grant job-run authority.
pub fn managed_dispatch_context_from_env() -> bool {
    let managed = std::env::var(ORBIT_MANAGED_RUN_CONTEXT_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"));
    managed
        && (managed_run_context_from_env()
            || std::env::var("ORBIT_SESSION_ID")
                .ok()
                .is_some_and(|value| non_empty(&value).is_some()))
}

/// Logical workspace selector carried by a managed child (`ORBIT_WORKSPACE`).
///
/// Honored only with Orbit's managed marker and a non-blank run or invocation
/// identity. Source-inspection providers have an invocation session but no job
/// run. A standalone process that merely inherits the selector must not bind.
pub fn managed_workspace_selector_from_env() -> Option<String> {
    if !managed_dispatch_context_from_env() {
        return None;
    }
    std::env::var("ORBIT_WORKSPACE")
        .ok()
        .and_then(|value| non_empty(&value).map(ToOwned::to_owned))
}

/// Extract the singular task id from run/activity input shapes that are meant
/// to identify exactly one task.
pub fn singular_task_id_from_input(input: &Value) -> Option<&str> {
    input
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .or_else(|| {
            input
                .get("task")
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .and_then(non_empty)
        })
        .or_else(|| {
            let items = input.get("task_ids")?.as_array()?;
            if items.len() == 1 {
                items.first()?.as_str().and_then(non_empty)
            } else {
                None
            }
        })
}

pub fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Task ids a run input carries: the `task_ids` array plus a singular `task_id`.
///
/// Each entry is trimmed and empties are dropped. The set is sorted and
/// deduplicated. `epic_task_id` and nested `task.id` are not read; callers
/// that need those keys add them themselves.
pub fn carried_task_ids(input: &Value) -> BTreeSet<String> {
    input
        .get("task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(input.get("task_id").and_then(Value::as_str))
        .filter_map(non_empty)
        .map(ToOwned::to_owned)
        .collect()
}

/// The recorded `task_ids` array, in input order, borrowed as stored.
///
/// Strings are not trimmed. Empties and duplicates are kept, and non-strings
/// are skipped. The singular `task_id` is not included. A missing array is empty.
pub fn recorded_task_ids(input: &Value) -> Vec<&str> {
    input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
