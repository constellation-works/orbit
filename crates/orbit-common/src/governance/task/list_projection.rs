//! The default per-task shape of `orbit.task.list`.
//!
//! Without a `fields` projection the list handler builds full task records, which
//! carry long text (description, plan, execution summary, acceptance criteria).
//! Core's `orbit.task.list` result policy narrows that to
//! [`MINIMAL_TASK_FIELDS`] for `orbit tool run` and MCP, so one tool name has
//! one default result shape.

use serde_json::{Map, Value};

/// The per-task fields of a default `orbit.task.list` row.
pub const MINIMAL_TASK_FIELDS: &[&str] = &[
    "id",
    "title",
    "status",
    "priority",
    "type",
    "dependencies",
    "resolved_dependencies",
    "implemented_by",
    "not_before",
    "created_at",
    "updated_at",
    "workspace",
];

/// Keeps only `fields` of `map`, in `fields` order; absent fields are skipped.
pub fn select_fields(map: Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = map.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    selected
}

/// Projects `fields` inside the `tasks` array of an `orbit.task.list`
/// envelope, leaving `total`/`truncated` intact.
pub fn project_task_list_output<S: AsRef<str>>(value: Value, fields: &[S]) -> Value {
    let Value::Object(mut object) = value else {
        return value;
    };
    let Some(Value::Array(tasks)) = object.get_mut("tasks") else {
        return Value::Object(object);
    };
    let fields = fields
        .iter()
        .map(|field| field.as_ref().to_string())
        .collect::<Vec<_>>();
    let projected = std::mem::take(tasks)
        .into_iter()
        .map(|task| match task {
            Value::Object(map) => Value::Object(select_fields(map, &fields)),
            other => other,
        })
        .collect();
    *tasks = projected;
    Value::Object(object)
}
