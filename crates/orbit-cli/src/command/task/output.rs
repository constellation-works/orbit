use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use orbit_core::{
    OrbitError, OrbitRuntime, TaskCrewRead, TaskStatus, resolve_task_dependencies,
    resolve_task_relations,
};
use orbit_types::task::{
    ArtifactManifestFileV2, TaskComment, TaskHistoryEntry, serialize_task_artifacts,
    task_show_record_field_json, unknown_task_show_field_message,
};
use serde_json::{Value, json};

use crate::output::color::Domain;

/// Legacy bare `commented` history entries duplicate the authoritative Comments
/// list, so human-facing history rendering omits them (ORB-10311). Raw JSON/MCP
/// history projections keep every event for backward compatibility.
pub(crate) fn is_human_visible_history_event(event: &str) -> bool {
    event != "commented"
}

pub(crate) fn task_to_signal_json(task: &orbit_core::Task) -> Value {
    json!({
        "id": task.id,
        "parent_id": task.parent_id(),
        "title": task.title,
        "type": task.task_type.to_string(),
        "status": task.status.to_string(),
        "priority": task.priority.to_string(),
        "complexity": task.complexity.map(|value| value.to_string()),
    })
}

pub(crate) fn task_to_json(
    task: &orbit_core::Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Value {
    json!({
        "id": task.id,
        "parent_id": task.parent_id(),
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "dependencies": task.dependencies(),
        "resolved_dependencies": dependency_labels(task, status_by_id),
        "tags": task.tags,
        "required_tools": task.required_tools,
        "plan": task.plan,
        "execution_summary": task.execution_summary,
        "context_files": task.context_files,
        "created_by": task.created_by,
        "planned_by": task.planned_by,
        "implemented_by": task.implemented_by,
        "status": task.status.to_string(),
        "priority": task.priority.to_string(),
        "complexity": task.complexity.map(|value| value.to_string()),
        "type": task.task_type.to_string(),
        "pr_status": task.pr_status,
        "external_refs": task.external_refs,
        "relations": resolve_task_relations(task, status_by_id),
        "source_task_id": task.source_task_id(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
    })
}

pub(crate) fn task_to_json_for_runtime(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
) -> Result<Value, OrbitError> {
    let status_by_id = runtime.task_status_index()?;
    Ok(task_to_json_with_sidecars(runtime, task, &status_by_id)?.doc)
}

pub(crate) struct TaskJsonWithSidecars {
    pub(crate) doc: Value,
    pub(crate) comments: Vec<TaskComment>,
    pub(crate) history: Vec<TaskHistoryEntry>,
}

pub(crate) fn task_to_json_with_sidecars(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<TaskJsonWithSidecars, OrbitError> {
    let mut value = task_to_json(task, status_by_id);
    let object = value.as_object_mut().ok_or_else(|| {
        OrbitError::Execution("task JSON projection did not produce an object".to_string())
    })?;
    let comments = runtime.get_task_comments(&task.id)?;
    let history = runtime.get_task_history(&task.id)?;
    object.insert(
        "comments".to_string(),
        serde_json::to_value(&comments).map_err(|e| OrbitError::Io(e.to_string()))?,
    );
    object.insert(
        "history".to_string(),
        serde_json::to_value(&history).map_err(|e| OrbitError::Io(e.to_string()))?,
    );
    let artifacts = runtime.get_task_artifact_manifest(&task.id)?;
    object.insert(
        "artifacts".to_string(),
        task_artifact_manifest_to_json(&artifacts),
    );
    // [ORB-11333] The settled before-PR review gate, when one exists, is
    // durable evidence and travels with the record like the artifacts.
    if let Some(review) =
        orbit_core::application::review::task_review_projection(runtime, task, &artifacts)?
    {
        object.insert("review".to_string(), review);
    }
    // `resolved_crew` enriches the record; it does not define it. A task may
    // name a crew this workspace has no `[crews.*]` entry for — a config gap,
    // not a corrupt task — and the fields are simply absent then.
    //
    // This used to propagate, which was invisible while only `--json` built
    // this projection. Since ORB-10586 every mode does (the payload is the
    // same records in both), so propagating would mean `orbit task show`
    // refusing to display a task whose crew is undefined here. ORB-10968 moved
    // the rule itself into `OrbitRuntime::task_crew_read` so the tool host and
    // MCP read the same contract; the reason still goes to stderr here rather
    // than being swallowed.
    match runtime.task_crew_read(task) {
        TaskCrewRead::Resolved(projection) => {
            object.insert("resolved_crew".to_string(), Value::String(projection.name));
            object.insert("crew_model".to_string(), Value::String(projection.model));
        }
        TaskCrewRead::Absent => {}
        TaskCrewRead::Unresolved { reason } => {
            object.insert("crew_unresolved".to_string(), Value::String(reason.clone()));
            eprintln!(
                "warning: crew for task {} could not be resolved: {reason}",
                task.id
            );
        }
    }
    Ok(TaskJsonWithSidecars {
        doc: value,
        comments,
        history,
    })
}

/// Which columns the caller filtered on, and so must keep even when the filter
/// left them carrying a single value.
#[derive(Clone, Copy, Default)]
pub(super) struct TaskTableFilters {
    pub(super) status: bool,
    pub(super) priority: bool,
    pub(super) task_type: bool,
}

/// Build the list view for `orbit task list`. The renderer decides how it is
/// projected; this only describes the columns and the rows.
pub(super) fn task_table(
    tasks: &[orbit_core::Task],
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
        let mut row = vec![
            Cell::new(&task.id),
            Cell::new(&task.title),
            crate::output::color::cell(&task.status.to_string(), Domain::TaskStatus),
            crate::output::color::cell(&task.priority.to_string(), Domain::Priority),
            crate::output::color::cell(&task.task_type.to_string(), Domain::TaskType),
        ];
        if full {
            row.extend([
                Cell::new(task.implemented_by.as_deref().unwrap_or("-")),
                Cell::new(format_task_table_timestamp(task.created_at)),
                Cell::new(format_task_table_timestamp(task.updated_at)),
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

pub(super) fn task_field_to_json(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
    field: &str,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<Value, OrbitError> {
    match field {
        "comments" => serde_json::to_value(runtime.get_task_comments(&task.id)?)
            .map_err(|e| OrbitError::Io(e.to_string())),
        "plan" => Ok(Value::String(task.plan.clone())),
        "execution_summary" => Ok(Value::String(task.execution_summary.clone())),
        "description" => Ok(Value::String(task.description.clone())),
        "acceptance_criteria" => serde_json::to_value(&task.acceptance_criteria)
            .map_err(|e| OrbitError::Io(e.to_string())),
        "dependencies" => {
            serde_json::to_value(task.dependencies()).map_err(|e| OrbitError::Io(e.to_string()))
        }
        "tags" => serde_json::to_value(&task.tags).map_err(|e| OrbitError::Io(e.to_string())),
        "required_tools" => {
            serde_json::to_value(&task.required_tools).map_err(|e| OrbitError::Io(e.to_string()))
        }
        "resolved_dependencies" => serde_json::to_value(dependency_labels(
            task,
            status_by_id.ok_or_else(|| {
                OrbitError::Execution(
                    "missing dependency status index for resolved_dependencies".to_string(),
                )
            })?,
        ))
        .map_err(|e| OrbitError::Io(e.to_string())),
        "relations" => serde_json::to_value(resolve_task_relations(
            task,
            status_by_id.ok_or_else(|| {
                OrbitError::Execution("missing task status index for relations".to_string())
            })?,
        ))
        .map_err(|e| OrbitError::Io(e.to_string())),
        "history" => serde_json::to_value(runtime.get_task_history(&task.id)?)
            .map_err(|e| OrbitError::Io(e.to_string())),
        "context_files" => {
            serde_json::to_value(&task.context_files).map_err(|e| OrbitError::Io(e.to_string()))
        }
        "crew" => serde_json::to_value(&task.crew).map_err(|e| OrbitError::Io(e.to_string())),
        "orchestrator" => {
            serde_json::to_value(&task.orchestrator).map_err(|e| OrbitError::Io(e.to_string()))
        }
        "artifacts" => Ok(serialize_task_artifacts(
            &runtime.get_task_artifact_manifest(&task.id)?,
        )),
        other => task_show_record_field_json(task, other)
            .ok_or_else(|| OrbitError::InvalidInput(unknown_task_show_field_message(other))),
    }
}

pub(super) fn task_fields_to_json(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
    fields: &[String],
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<Value, OrbitError> {
    if fields.len() == 1 {
        return task_field_to_json(runtime, task, &fields[0], status_by_id);
    }

    let mut object = serde_json::Map::new();
    for field in fields {
        object.insert(
            field.clone(),
            task_field_to_json(runtime, task, field, status_by_id)?,
        );
    }
    Ok(Value::Object(object))
}

pub(super) fn format_task_fields(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
    fields: &[String],
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<String, OrbitError> {
    let mut text = String::new();
    if fields.len() == 1 {
        write_single_task_field(runtime, task, &fields[0], status_by_id, &mut text)?;
        return Ok(text);
    }

    use crate::output::color::bold;
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let _ = writeln!(text, "{} {}", bold("Field:"), field);
        write_single_task_field(runtime, task, field, status_by_id, &mut text)?;
    }
    Ok(text)
}

fn write_single_task_field(
    runtime: &OrbitRuntime,
    task: &orbit_core::Task,
    field: &str,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
    text: &mut String,
) -> Result<(), OrbitError> {
    match field {
        "comments" => {
            use crate::output::color::dimmed;
            for comment in runtime.get_task_comments(&task.id)? {
                let _ = writeln!(
                    text,
                    "{} {}: {}",
                    dimmed(&format!("[{}]", comment.at.to_rfc3339())),
                    comment.by,
                    comment.message
                );
            }
            Ok(())
        }
        "plan" => {
            text.push_str(&task.plan);
            Ok(())
        }
        "execution_summary" => {
            text.push_str(&task.execution_summary);
            Ok(())
        }
        "description" => {
            text.push_str(&task.description);
            Ok(())
        }
        "acceptance_criteria" => {
            for criterion in &task.acceptance_criteria {
                let _ = writeln!(text, "- {}", criterion);
            }
            Ok(())
        }
        "dependencies" => {
            for dependency in task.dependencies() {
                let _ = writeln!(text, "{}", dependency);
            }
            Ok(())
        }
        "tags" => {
            for tag in &task.tags {
                let _ = writeln!(text, "{}", tag);
            }
            Ok(())
        }
        "required_tools" => {
            for tool in &task.required_tools {
                let _ = writeln!(text, "{}", tool);
            }
            Ok(())
        }
        "resolved_dependencies" => {
            for dependency in dependency_labels(
                task,
                status_by_id.ok_or_else(|| {
                    OrbitError::Execution(
                        "missing dependency status index for resolved_dependencies".to_string(),
                    )
                })?,
            ) {
                let _ = writeln!(text, "{}", dependency);
            }
            Ok(())
        }
        "relations" => {
            for relation in resolve_task_relations(
                task,
                status_by_id.ok_or_else(|| {
                    OrbitError::Execution("missing task status index for relations".to_string())
                })?,
            ) {
                let _ = writeln!(
                    text,
                    "{}",
                    serde_json::to_string(&relation).map_err(|e| OrbitError::Io(e.to_string()))?
                );
            }
            Ok(())
        }
        "history" => {
            use crate::output::color::dimmed;
            for entry in runtime.get_task_history(&task.id)? {
                if !is_human_visible_history_event(&entry.event) {
                    continue;
                }
                if let Some(note) = &entry.note {
                    let _ = writeln!(
                        text,
                        "{} {}: {} ({})",
                        dimmed(&format!("[{}]", entry.at.to_rfc3339())),
                        entry.by,
                        entry.event,
                        note
                    );
                } else {
                    let _ = writeln!(
                        text,
                        "{} {}: {}",
                        dimmed(&format!("[{}]", entry.at.to_rfc3339())),
                        entry.by,
                        entry.event
                    );
                }
            }
            Ok(())
        }
        "context_files" => {
            for path in &task.context_files {
                let _ = writeln!(text, "{}", path);
            }
            Ok(())
        }
        "crew" => {
            text.push_str(task.crew.as_deref().unwrap_or_default());
            Ok(())
        }
        "orchestrator" => {
            text.push_str(task.orchestrator.as_deref().unwrap_or_default());
            Ok(())
        }
        "artifacts" => {
            use crate::output::color::bold;
            let artifacts = runtime.get_task_artifact_manifest(&task.id)?;
            for (index, artifact) in artifacts.iter().enumerate() {
                if index > 0 {
                    text.push('\n');
                }
                let _ = writeln!(
                    text,
                    "{} {} ({}, {} bytes)",
                    bold("Artifact:"),
                    artifact.path,
                    artifact.media_type,
                    artifact.size_bytes
                );
            }
            Ok(())
        }
        other => match task_show_record_field_json(task, other) {
            Some(Value::String(value)) => {
                text.push_str(&value);
                Ok(())
            }
            Some(Value::Null) => Ok(()),
            Some(value) => {
                let _ = write!(text, "{value}");
                Ok(())
            }
            None => Err(OrbitError::InvalidInput(unknown_task_show_field_message(
                other,
            ))),
        },
    }
}

pub(crate) fn task_artifact_manifest_to_json(files: &[ArtifactManifestFileV2]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|file| {
                json!({
                    "path": file.path,
                    "media_type": file.media_type,
                    "size_bytes": file.size_bytes,
                    "sha256": file.sha256,
                    "created_by": file.created_by,
                    "created_at": file.created_at.to_rfc3339(),
                })
            })
            .collect(),
    )
}

fn dependency_labels(
    task: &orbit_core::Task,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Vec<String> {
    resolve_task_dependencies(task, status_by_id)
        .into_iter()
        .map(|dependency| dependency.label())
        .collect()
}

fn format_task_table_timestamp(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M").to_string()
}
