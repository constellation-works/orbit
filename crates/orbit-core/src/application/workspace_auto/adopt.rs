//! The drain step that adopts interrupted delivery leaves' open pull
//! requests before it admits new work. What qualifies is
//! `application::job::orphan_adoption`'s decision; this step only supplies
//! the drain's authority and its free leaf slots.

use chrono::Utc;
use orbit_engine::DispatchError;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::AdoptionAuthority;

use super::classify::templated_u64;
use super::drains::{DEFAULT_MAX_ACTIVE_LEAF_RUNS, live_admissions_stop, live_worker_limit};

pub(crate) fn adopt_orphaned_deliveries(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let Some(drain_run_id) = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(json!({ "adopted": [], "skipped": [] }));
    };
    // A stopped drain or a host about to shut down starts nothing, as for
    // admission.
    if live_admissions_stop(runtime, input).is_some()
        || runtime.host_shutdown_admission_hold().is_some()
    {
        return Ok(json!({ "adopted": [], "skipped": [] }));
    }
    let submitted = templated_u64(
        action,
        input,
        "max_active_leaf_runs",
        DEFAULT_MAX_ACTIVE_LEAF_RUNS,
    )?;
    let ceiling = live_worker_limit(runtime, input)
        .map_or(submitted, |limit| u64::from(limit.max_active_leaf_runs));
    let occupied = runtime
        .stores()
        .jobs()
        .drain_leaf_occupancy_for_run(drain_run_id)
        .map_err(|error| {
            DispatchError::action_failed(action, format!("read shared leaf occupancy: {error}"))
        })?
        .occupied;
    let authority = AdoptionAuthority {
        drain_run_id,
        completion_done: input.get("completion").and_then(Value::as_str) == Some("done"),
        auto_fix: templated_bool(input.get("auto_fix")),
        budget: usize::try_from(ceiling)
            .unwrap_or(usize::MAX)
            .saturating_sub(occupied),
    };
    runtime
        .adopt_orphaned_deliveries(&authority, Utc::now())
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))
}

/// A templated flag: the template engine may hand a boolean over as text.
fn templated_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}
