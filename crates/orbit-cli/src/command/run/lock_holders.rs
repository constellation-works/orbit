//! Who holds the locks a run is waiting on, for `orbit run show`.
//!
//! A gate run records only the selectors it is blocked on (`waiting_on_locks`),
//! not who holds them, so the holder is resolved at render time from the live
//! lock projection. Nothing persisted changes: a holder that has since
//! released simply stops appearing.

use std::collections::BTreeSet;

use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_core::{JobRun, OrbitRuntime};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use super::format::LockHolders;

/// Holders of every selector a non-terminal run is waiting on. Empty for a
/// run that waits on no lock, or when the projection cannot be read.
pub(super) fn waiting_lock_holders(
    runtime: &OrbitRuntime,
    run: &JobRun,
    state: Option<&PipelineState>,
) -> LockHolders {
    let selectors = match state.filter(|_| !run.state.is_terminal()) {
        Some(state) => state.waiting_on_locks.clone().unwrap_or_default(),
        None => Vec::new(),
    };
    if selectors.is_empty() {
        return LockHolders::new();
    }
    let Ok(locks) = runtime.run_tool("orbit.task.locks", json!({})) else {
        return LockHolders::new();
    };
    resolve_lock_holders(&selectors, &locks, &run_task_ids(run))
}

/// The tasks this run itself carries: they never count as their own holder.
fn run_task_ids(run: &JobRun) -> BTreeSet<String> {
    let Some(input) = run.input.as_ref() else {
        return BTreeSet::new();
    };
    let listed = input
        .get("task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let single = input.get("task_id").and_then(Value::as_str);
    listed.chain(single).map(str::to_string).collect()
}

/// Match each waited-on selector against the projection's active task locks
/// and reservations, skipping `own` tasks.
pub(super) fn resolve_lock_holders(
    selectors: &[String],
    locks: &Value,
    own: &BTreeSet<String>,
) -> LockHolders {
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    // (held selector, holder task ids)
    let mut held: Vec<(String, Vec<String>)> = Vec::new();
    for task in locks["by_task"].as_array().into_iter().flatten() {
        let Some(id) = task["id"].as_str() else {
            continue;
        };
        for file in strings(&task["context_files"]) {
            held.push((file, vec![id.to_string()]));
        }
    }
    for reservation in locks["by_reservation"].as_array().into_iter().flatten() {
        let holders = strings(&reservation["task_ids"]);
        for file in strings(&reservation["files"]) {
            held.push((file, holders.clone()));
        }
    }

    let mut resolved = LockHolders::new();
    for selector in selectors {
        let holders = held
            .iter()
            .filter(|(held_selector, _)| workspace_relative_paths_overlap(selector, held_selector))
            .flat_map(|(_, tasks)| tasks.iter())
            .filter(|task| !own.contains(*task))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !holders.is_empty() {
            resolved.insert(selector.clone(), holders.into_iter().collect());
        }
    }
    resolved
}
