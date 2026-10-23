//! Lock-denial extraction and stable requester identities.

use std::collections::BTreeMap;

use orbit_store::contracts::TaskLockDenialRecord;
use serde_json::Value;

use super::scan::LockDenial;
use super::types::Anchor;

/// A lock-conflict denial from `orbit.task.locks.reserve`. Capability
/// refusals and denials without a conflict are not contention.
/// `reservation_tasks` names the tasks of each recorded reservation, by id.
pub(super) fn lock_denial(
    event: &TaskLockDenialRecord,
    reservation_tasks: &BTreeMap<String, Vec<String>>,
) -> Option<LockDenial> {
    let arguments: Value = serde_json::from_str(&event.arguments_json).ok()?;
    let strings = |key: &str| -> Vec<String> {
        arguments
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    };
    let conflicts = arguments.get("conflicts").and_then(Value::as_array)?;
    if conflicts.is_empty() {
        return None;
    }
    let mut files: Vec<Anchor> = conflicts
        .iter()
        .filter_map(|conflict| conflict.get("file").and_then(Value::as_str))
        .filter_map(Anchor::parse)
        .collect();
    if files.is_empty() {
        files = strings("files")
            .iter()
            .filter_map(|f| Anchor::parse(f))
            .collect();
    }
    files.sort();
    files.dedup();
    let mut holders: Vec<(Anchor, String)> = conflicts
        .iter()
        .filter_map(|conflict| {
            let file = Anchor::parse(conflict.get("file")?.as_str()?)?;
            let id = conflict.get("held_by_id")?.as_str()?;
            let tasks = match conflict.get("held_by")?.as_str()? {
                "task" => vec![id.to_string()],
                "reservation" => reservation_tasks.get(id).cloned().unwrap_or_default(),
                _ => Vec::new(),
            };
            Some(tasks.into_iter().map(move |task| (file.clone(), task)))
        })
        .flatten()
        .collect();
    holders.sort();
    holders.dedup();
    let run = arguments
        .get("owner_run_id")
        .and_then(Value::as_str)
        .filter(|run| !run.is_empty())
        .map(str::to_string)
        .or_else(|| event.job_run_id.clone());
    let task_ids = strings("task_ids");
    let contenders = requester_identities(
        task_ids.clone(),
        run,
        format!("invocation:{}", event.execution_id),
    );
    Some(LockDenial {
        at: event.timestamp,
        contenders,
        task_ids,
        files,
        holders,
    })
}

/// Task retries retain their task identity across runs. Requests without
/// tasks use the run, or one recorded request when no run is known.
pub(super) fn requester_identities(
    task_ids: Vec<String>,
    run: Option<String>,
    fallback: String,
) -> Vec<String> {
    if !task_ids.is_empty() {
        return task_ids;
    }
    vec![
        run.filter(|run| !run.is_empty())
            .map(|run| format!("run:{run}"))
            .unwrap_or(fallback),
    ]
}
