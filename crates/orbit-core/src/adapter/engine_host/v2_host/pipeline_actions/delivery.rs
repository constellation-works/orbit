//! `resolve_delivery_job`: the job a gated bundle is delivered by.

use orbit_engine::DispatchError;
use orbit_types::workflow::ShipMode;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::task::locks::parse_task_ids;

use super::action_failed;

/// Translate the gate's `{task_ids, mode}` into the resolved delivery route.
/// The decision is the application's
/// ([`OrbitRuntime::resolve_admitted_delivery_route`]); a refusal — including
/// a PR route with no forge remote — fails the step before the gate reserves
/// anything or creates a worktree.
pub(in super::super) fn resolve_delivery_job(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let task_ids =
        parse_task_ids(input).map_err(|error| action_failed(action, error.to_string()))?;
    let mode = input
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| action_failed(action, "missing `mode`".to_string()))
        .and_then(|mode| {
            ShipMode::parse(mode.trim()).map_err(|error| action_failed(action, error.to_string()))
        })?;
    let tasks = task_ids
        .iter()
        .map(|task_id| {
            runtime
                .get_task(task_id)
                .map_err(|error| action_failed(action, format!("load task {task_id}: {error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let route = runtime
        .resolve_admitted_delivery_route(
            &tasks,
            mode,
            &crate::application::job::delivery::PrForgeCheck::default(),
        )
        .map_err(|error| action_failed(action, error.to_string()))?;
    let mut output = serde_json::json!({
        "job_name": route.job_name,
        "selected": route.selected,
    });
    if let Some(plugin) = route.plugin {
        output["plugin"] = Value::String(plugin);
    }
    Ok(output)
}
