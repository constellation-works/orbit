//! Run-input task-id extraction and managed-run environment marker re-exports.

use std::collections::BTreeSet;

use serde_json::Value;

pub use crate::base::managed_env::{
    ORBIT_MANAGED_RUN_CONTEXT_ENV, managed_dispatch_context_from_env, managed_run_context_from_env,
    managed_run_context_run_id_from_env, managed_run_context_task_id_from_env,
    managed_workspace_selector_from_env, non_empty,
};
pub use crate::host::resource::delivery::singular_task_id_from_input;

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
