use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_core::application::task::task_record_json;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::TaskHistoryEntry;
use serde_json::{Value, json};

use crate::output::color::Domain;

/// Legacy bare `commented` history entries duplicate the authoritative Comments
/// list, so human-facing history rendering omits them (ORB-10311). Raw JSON/MCP
/// history projections keep every event for backward compatibility.
pub(crate) fn is_human_visible_history_event(event: &str) -> bool {
    event != "commented"
}

pub(super) fn format_task_show_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(super) fn format_task_history_event(entry: &TaskHistoryEntry) -> String {
    match (
        entry.event.as_str(),
        entry.from_status.as_ref(),
        entry.to_status.as_ref(),
    ) {
        ("status_changed", Some(from), Some(to)) => format!("status {from} → {to}"),
        ("started", Some(from), Some(to)) => format!("started {from} → {to}"),
        _ => entry.event.clone(),
    }
}

pub(crate) fn task_to_json_for_runtime(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
) -> Result<Value, OrbitError> {
    let status_by_id = runtime.dependency_status_index([task])?;
    let row = runtime.task_row_with_sidecars(task.clone())?;
    let doc = task_record_json(runtime, &row, &status_by_id)?;
    warn_unresolved_crew(&doc);
    Ok(Value::Object(doc))
}

/// A crew this host cannot resolve is a configuration gap, not a corrupt
/// task, so the record still renders (ORB-10968); the reason also goes to
/// stderr rather than being swallowed.
pub(super) fn warn_unresolved_crew(doc: &serde_json::Map<String, Value>) {
    if let Some(reason) = doc.get("crew_unresolved").and_then(Value::as_str) {
        let id = doc.get("id").and_then(Value::as_str).unwrap_or_default();
        eprintln!("warning: crew for task {id} could not be resolved: {reason}");
    }
}

/// A task write's document: the stored task's full record, carrying the
/// write's `warnings` and `context_files_unverified` selectors from the
/// tool's answer when there are any, and its redactions when it applied
/// any. Each warning is also printed to stderr so a text-mode caller sees it.
pub(super) fn task_write_json(
    runtime: &OrbitRuntime,
    answer: &Value,
) -> Result<(Value, orbit_core::Task), OrbitError> {
    let id = answer.get("id").and_then(Value::as_str).ok_or_else(|| {
        OrbitError::Execution("task write answer did not name the task".to_string())
    })?;
    let task = runtime.get_task(id)?;
    let mut document = task_to_json_for_runtime(runtime, &task)?;
    let strings = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let warnings = strings("warnings");
    let unverified = strings("context_files_unverified");
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    if !unverified.is_empty() {
        eprintln!(
            "warning: context selectors stored without a verified target: {}",
            unverified.join(", ")
        );
    }
    if let Some(object) = document.as_object_mut() {
        if !unverified.is_empty() {
            object.insert("context_files_unverified".to_string(), json!(unverified));
        }
        if !warnings.is_empty() {
            object.insert("warnings".to_string(), json!(warnings));
        }
        if answer.get("redactions_applied") == Some(&Value::Bool(true)) {
            object.insert("redactions_applied".to_string(), Value::Bool(true));
            object.insert(
                "redactions".to_string(),
                answer.get("redactions").cloned().unwrap_or(Value::Null),
            );
        }
    }
    Ok((document, task))
}

/// Which columns the caller filtered on, and so must keep even when the filter
/// left them carrying a single value.
#[derive(Clone, Copy, Default)]
pub(super) struct TaskTableFilters {
    pub(super) status: bool,
    pub(super) priority: bool,
    pub(super) task_type: bool,
    pub(super) runs_on: bool,
}

/// Build the list view for `orbit task list` from the tool's task records.
/// The renderer decides how it is projected; this only describes the columns
/// and the rows.
pub(super) fn task_table(
    tasks: &[Value],
    full: bool,
    filtered: TaskTableFilters,
) -> crate::output::table::Table {
    use crate::output::table::Column;
    use comfy_table::Cell;
    // `orbit task show <id>` prints the untruncated title and body of any row.
    let mut columns = vec![
        Column::new("ID").fixed(),
        Column::new("TITLE"),
        Column::new("STATUS").fixed().filtered(filtered.status),
        Column::new("PRIORITY").fixed().filtered(filtered.priority),
        Column::new("TYPE").fixed().filtered(filtered.task_type),
        Column::new("RUNS ON").filtered(
            filtered.runs_on
                || tasks
                    .iter()
                    .any(|task| task.get("os_requirement").is_some()),
        ),
    ];
    if full {
        columns.extend([
            Column::new("IMPLEMENTED_BY").fixed(),
            Column::new("CREATED_AT").fixed(),
            Column::new("UPDATED_AT").fixed(),
        ]);
    }
    let mut table = crate::output::table::Table::new(columns)
        .empty_message("no tasks matching the given filters");
    for task in tasks {
        let field = |key: &str| task.get(key).and_then(Value::as_str).unwrap_or_default();
        let mut row = vec![
            Cell::new(field("id")),
            Cell::new(field("title")),
            crate::output::color::cell(field("status"), Domain::TaskStatus),
            crate::output::color::cell(field("priority"), Domain::Priority),
            crate::output::color::cell(field("type"), Domain::TaskType),
            Cell::new(
                task.get("os_requirement")
                    .and_then(|os| os.get("describe"))
                    .and_then(Value::as_str)
                    .unwrap_or(if field("crew").is_empty() {
                        "any host from OS tags (crew unassigned)"
                    } else {
                        "any host"
                    }),
            ),
        ];
        if full {
            row.extend([
                Cell::new(
                    task.get("implemented_by")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                ),
                Cell::new(format_task_table_timestamp(field("created_at"))),
                Cell::new(format_task_table_timestamp(field("updated_at"))),
            ]);
        }
        table.add_row(row);
    }
    table
}

/// Render `orbit task locks list`'s human-readable view from the same
/// `orbit.task.locks` projection `--json` prints verbatim, so the two
/// surfaces cannot disagree about what a reservation is.
pub(crate) fn format_task_locks(locks: &Value) -> String {
    let by_task = locks["by_task"].as_array().map_or(&[][..], Vec::as_slice);
    let by_reservation = locks["by_reservation"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);

    let mut out = String::new();
    if by_task.is_empty() && by_reservation.is_empty() {
        out.push_str("No files currently locked.\n");
        return out;
    }

    for (index, task) in by_task.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }

        let id = task["id"].as_str().unwrap_or_default();
        let title = task["title"].as_str().unwrap_or_default();
        let status = task["status"].as_str().unwrap_or_default();
        match task["job_run_id"].as_str() {
            Some(job_run_id) => {
                let _ = writeln!(out, "[{id}] {title} ({status}, job_run={job_run_id})");
            }
            None => {
                let _ = writeln!(out, "[{id}] {title} ({status})");
            }
        }

        for path in task["context_files"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            if let Some(path) = path.as_str() {
                let _ = writeln!(out, "  - {path}");
            }
        }
    }

    let total_locked = locks["total_locked"].as_u64().unwrap_or(0);
    let total_tasks = locks["total_tasks"].as_u64().unwrap_or(0);
    let _ = writeln!(
        out,
        "\n{total_locked} file(s) locked across {total_tasks} task(s)."
    );

    if !by_reservation.is_empty() {
        out.push_str("\nReservations:\n");
        for reservation in by_reservation {
            let id = reservation["reservation_id"].as_str().unwrap_or_default();
            let task_ids = json_str_list(&reservation["task_ids"]);
            let files = json_str_list(&reservation["files"]);
            let expires_at = reservation["expires_at"].as_str().unwrap_or_default();
            let _ = writeln!(
                out,
                "[{id}] tasks=[{task_ids}] files=[{files}] expires_at={expires_at}"
            );
        }
    }

    out
}

fn json_str_list(value: &Value) -> String {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn format_task_table_timestamp(value: &str) -> String {
    parse_timestamp(value).map_or_else(
        || value.to_string(),
        |at| at.format("%Y-%m-%d %H:%M").to_string(),
    )
}

/// A record timestamp as the tool serializes it (RFC 3339).
pub(super) fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}
