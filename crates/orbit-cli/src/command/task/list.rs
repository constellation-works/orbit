//! `orbit task list` rendering.

use orbit_core::OrbitError;
use serde_json::{Map, Value};

use super::operation::ListView;
use super::output::{TaskTableFilters, task_table};
use crate::command::{CommandOut, Payload};

/// The keys of a signal-tier (`--ops`) record, in output order.
const SIGNAL_FIELDS: [&str; 7] = [
    "id",
    "parent_id",
    "title",
    "type",
    "status",
    "priority",
    "complexity",
];

pub(super) fn render(input: &Map<String, Value>, value: Value, view: ListView) -> CommandOut {
    if let Some(count) = value.get("unindexed_bundle_count").and_then(Value::as_u64) {
        return Err(OrbitError::Store(format!(
            "task index is missing {count} on-disk bundle(s); run `orbit task reindex` to recover them"
        )));
    }
    let tasks = value
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = value.get("total").and_then(Value::as_u64).unwrap_or(0);
    let status_filtered = input.contains_key("status");
    // A column the caller filtered on stays on screen even when the filter
    // made it uniform.
    let filtered = TaskTableFilters {
        status: status_filtered,
        priority: input.contains_key("priority"),
        task_type: input.contains_key("type"),
        runs_on: input.contains_key("runs_on"),
    };

    let mut table = task_table(&tasks, view.full, filtered);
    if value.get("truncated").and_then(Value::as_bool) == Some(true) {
        // The default listing is status-aware (non-terminal tasks, then
        // terminal), so "newest first" only holds within each bucket, not
        // across the whole result; an explicit `--status` filter runs one
        // query and is newest first throughout (ORB-12200).
        let ordering = if status_filtered {
            "newest first"
        } else {
            "non-terminal tasks first, then terminal, each newest first"
        };
        table = table.trailing_notice(format!(
            "showing {} of {total} tasks ({ordering}); use --limit N or a filter to see more",
            tasks.len()
        ));
    }

    // `--ops` selects a narrower record shape, not a different output
    // channel: the table is the same either way, and the renderer decides
    // whether the caller sees records or rows.
    let records = if view.ops {
        tasks.iter().map(signal_record).collect()
    } else {
        tasks
    };
    Ok(Payload::list(records, table).into())
}

fn signal_record(task: &Value) -> Value {
    Value::Object(
        SIGNAL_FIELDS
            .iter()
            .map(|field| {
                (
                    (*field).to_string(),
                    task.get(*field).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}
