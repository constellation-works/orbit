use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, optional_string_alias,
    optional_string_list_alias, required_string,
};
use orbit_types::task::{TaskPriority, validate_relative_artifact_path};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams, compute_task_add_warnings};

use super::input::{
    empty_string_to_none, optional_bool_alias, parse_artifacts, parse_assessed_task_complexity,
    parse_relations, parse_task_priority, parse_task_status, parse_task_type,
};
use super::json::{
    serialize_task, serialize_task_artifact_read, serialize_task_lint_report, task_fields_to_json,
    task_to_json,
};

pub(super) fn add(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let title = required_string(&input, &["title"], "title")?;
    let description = required_string(&input, &["description"], "description")?;
    // `workspace` is required for the existing MCP/CLI routing that selects
    // this runtime before dispatch reaches here; context selectors always
    // canonicalize against the repository root regardless of its value.
    let _ = required_string(&input, &["workspace"], "workspace")?;
    let raw_context_files =
        optional_csv_or_string_list_alias(&input, &["context_files"])?.unwrap_or_default();
    if !allows_missing_context(&input)? {
        runtime.ensure_context_selectors_exist(&raw_context_files)?;
    }
    let task = runtime.add_task_with_identity(
        TaskAddParams {
            parent_id: None,
            title,
            description,
            acceptance_criteria: optional_string_list_alias(
                &input,
                &[
                    "acceptance_criteria",
                    "acceptanceCriteria",
                    "acceptance-criteria",
                ],
            )?
            .unwrap_or_default(),
            dependencies: Vec::new(),
            relations: parse_relations(&input)?.unwrap_or_default(),
            tags: optional_csv_or_string_list_alias(&input, &["tags", "tag"])?.unwrap_or_default(),
            required_tools: optional_csv_or_string_list_alias(
                &input,
                &["required_tools", "requiredTools", "required-tool"],
            )?
            .unwrap_or_default(),
            plan: String::new(),
            comment: None,
            context_files: raw_context_files.clone(),
            priority: optional_string(&input, "priority")?
                .map(|value| parse_task_priority("priority", &value))
                .transpose()?
                .unwrap_or(TaskPriority::Medium),
            complexity: parse_assessed_task_complexity(
                "complexity",
                &required_string(&input, &["complexity"], "complexity")?,
            )?,
            task_type: optional_string_alias(&input, &["type", "task_type", "taskType"])?
                .map(|value| parse_task_type("type", &value))
                .transpose()?,
            status: None,
            system_created: false,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: optional_string(&input, "crew")?,
            orchestrator: optional_string(&input, "orchestrator")?,
        },
        agent,
        model,
    )?;
    let mut response = serialize_task(runtime, &task)?;
    let warnings = compute_task_add_warnings(&raw_context_files, task.task_type);
    if !warnings.is_empty()
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert("warnings".to_string(), json!(warnings));
    }
    Ok(response)
}

pub(super) fn approve(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let task = runtime.approve_task_with_identity(
        &id,
        optional_string(&input, "note")?,
        optional_string(&input, "comment")?,
        agent,
        model,
    )?;
    serialize_task(runtime, &task)
}

pub(super) fn delete(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let force = optional_bool_alias(&input, &["force"])?.unwrap_or(false);
    runtime.delete_task_guarded(&id, force)?;
    Ok(json!({ "id": id, "deleted": true }))
}

pub(super) fn lint(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    serialize_task_lint_report(&runtime.lint_task(&id)?)
}

pub(super) fn list(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let statuses = optional_csv_or_string_list_alias(&input, &["status"])?
        .map(|values| {
            values
                .into_iter()
                .map(|value| parse_task_status("status", &value))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .filter(|values| !values.is_empty());
    let task_type = optional_string_alias(&input, &["type", "task_type", "taskType"])?
        .map(|value| parse_task_type("type", &value))
        .transpose()?;
    let parent_id = optional_string_alias(&input, &["parent_id", "parent", "parentId"])?;
    let job_run_id = optional_string(&input, "job_run_id")?;
    let tags = optional_csv_or_string_list_alias(&input, &["tags", "tag"])?.unwrap_or_default();
    let ready = optional_bool_alias(&input, &["ready"])?;
    let path = optional_string(&input, "path")?;
    let limit = super::input::task_list_limit(&input)?;
    let page = runtime.query_task_rows_status_aware(&crate::application::task::TaskListQuery {
        filter: crate::application::task::TaskListFilter {
            statuses,
            task_type,
            parent_id,
            job_run_id,
            tags,
            ..Default::default()
        },
        ready: ready == Some(true),
        path,
        limit,
    })?;
    let status_by_id = page.status_by_id;
    let tasks = page
        .items
        .into_iter()
        .map(|row| task_to_json(&row.task, &status_by_id))
        .collect::<Vec<_>>();
    let total = page.total;
    let truncated = tasks.len() < total;
    Ok(json!({
        "tasks": tasks,
        "total": total,
        "truncated": truncated,
    }))
}

pub(super) fn reject(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let note = required_string(&input, &["note"], "note")?;
    let task = runtime.reject_task_with_identity(
        &id,
        note,
        optional_string(&input, "comment")?,
        agent,
        model,
    )?;
    serialize_task(runtime, &task)
}

pub(super) fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let task = runtime.get_task(&id)?;
    let fields = optional_csv_or_string_list_alias(&input, &["fields", "field"])?;
    let with_context =
        optional_bool_alias(&input, &["with_context", "withContext", "with-context"])?
            .unwrap_or(false);
    let max_docs = optional_usize_alias(&input, &["max_docs", "maxDocs", "max-docs"])?;
    if let Some(fields) = fields {
        if with_context {
            return Err(OrbitError::InvalidInput(
                "`with_context` cannot be combined with `fields`".to_string(),
            ));
        }
        task_fields_to_json(runtime, &task, &fields)
    } else if with_context {
        let mut value = serialize_task(runtime, &task)?;
        let object = value.as_object_mut().ok_or_else(|| {
            OrbitError::Execution("task JSON projection did not produce an object".to_string())
        })?;
        object.insert(
            "related_docs".to_string(),
            serde_json::to_value(runtime.related_docs_for_task(&task, max_docs)?).map_err(
                |error| OrbitError::Execution(format!("serialize related docs: {error}")),
            )?,
        );
        Ok(value)
    } else {
        serialize_task(runtime, &task)
    }
}

fn optional_usize_alias(input: &Value, names: &[&str]) -> Result<Option<usize>, OrbitError> {
    for name in names {
        let Some(value) = input.get(*name) else {
            continue;
        };
        return match value {
            Value::Number(number) => number
                .as_u64()
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!("`{name}` must be an unsigned integer"))
                })
                .and_then(|value| {
                    usize::try_from(value)
                        .map(Some)
                        .map_err(|_| OrbitError::InvalidInput(format!("`{name}` is too large")))
                }),
            Value::String(raw) => raw.trim().parse::<usize>().map(Some).map_err(|error| {
                OrbitError::InvalidInput(format!("`{name}` must be an unsigned integer: {error}"))
            }),
            _ => Err(OrbitError::InvalidInput(format!(
                "`{name}` must be an unsigned integer"
            ))),
        };
    }
    Ok(None)
}

/// Read one stored artifact's bytes through the task's own artifact owner.
///
/// Discovery stays separate from retrieval: `orbit.task.show` with
/// `field: "artifacts"` lists compact metadata, and only this call pays for a
/// payload. Path containment, workspace ownership, and the symlink-safe blob
/// resolve all belong to the store, so this handler adds no second access
/// rule of its own — a caller can only ever reach an artifact through the task
/// that owns it.
pub(super) fn artifact_get(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let path = required_string(&input, &["path", "artifact_path", "artifactPath"], "path")?;
    validate_relative_artifact_path(&path)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    // Resolve the task first so an unknown or foreign id fails as not-found
    // before any artifact lookup reports on a task the caller cannot see.
    let task = runtime.get_task(&id)?;
    let artifact = runtime.get_task_artifact(&task.id, &path)?.ok_or_else(|| {
        OrbitError::not_found(
            orbit_common::NotFoundKind::Artifact,
            format!("{}/{path}", task.id),
        )
    })?;
    serialize_task_artifact_read(&task.id, &artifact)
}

pub(super) fn start(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let task = runtime.start_task_with_identity_and_crew(
        &id,
        optional_string(&input, "note")?,
        optional_string(&input, "comment")?,
        agent,
        model,
        optional_string(&input, "crew")?,
    )?;
    serialize_task(runtime, &task)
}

pub(super) fn update(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    owner: Option<orbit_tools::ReservationOwnerContext>,
) -> Result<Value, OrbitError> {
    if ["required_tools", "requiredTools", "required-tool"]
        .iter()
        .any(|field| input.get(*field).is_some())
    {
        return Err(OrbitError::InvalidInput(
            "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation"
                .to_string(),
        ));
    }
    let id = required_string(&input, &["id"], "id")?;
    let context_files = optional_csv_or_string_list_alias(&input, &["context_files", "context"])?;
    if !allows_missing_context(&input)?
        && let Some(candidates) = context_files.as_deref()
    {
        runtime.ensure_context_selectors_exist(candidates)?;
    }
    let task = runtime.update_task_with_owner(
        &id,
        TaskUpdateParams {
            title: optional_string(&input, "title")?,
            description: input
                .get("description")
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        OrbitError::InvalidInput("`description` must be a string".to_string())
                    })
                })
                .transpose()?,
            acceptance_criteria: optional_string_list_alias(
                &input,
                &[
                    "acceptance_criteria",
                    "acceptanceCriteria",
                    "acceptance-criteria",
                ],
            )?,
            dependencies: optional_csv_or_string_list_alias(&input, &["dependencies"])?,
            relations: parse_relations(&input)?,
            tags: optional_csv_or_string_list_alias(&input, &["tags", "tag"])?,
            plan: input
                .get("plan")
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        OrbitError::InvalidInput("`plan` must be a string".to_string())
                    })
                })
                .transpose()?,
            execution_summary: optional_raw_string(&input, "execution_summary")?,
            comment: optional_string(&input, "comment")?,
            status: optional_string(&input, "status")?
                .map(|value| parse_task_status("status", &value))
                .transpose()?,
            priority: optional_string(&input, "priority")?
                .map(|value| parse_task_priority("priority", &value))
                .transpose()?,
            complexity: optional_string(&input, "complexity")?
                .map(|value| parse_assessed_task_complexity("complexity", &value))
                .transpose()?,
            task_type: optional_string_alias(&input, &["type", "task_type", "taskType"])?
                .map(|value| parse_task_type("type", &value))
                .transpose()?,
            source_task_id: optional_raw_string_alias(
                &input,
                &["source_task_id", "source_task", "sourceTaskId"],
            )?
            .map(empty_string_to_none),
            planned_by: optional_raw_string(&input, "planned_by")?.map(empty_string_to_none),
            implemented_by: optional_raw_string(&input, "implemented_by")?
                .map(empty_string_to_none),
            pr_status: optional_raw_string(&input, "pr_status")?.map(empty_string_to_none),
            job_run_id: optional_raw_string(&input, "job_run_id")?.map(empty_string_to_none),
            crew: optional_raw_string(&input, "crew")?.map(empty_string_to_none),
            orchestrator: optional_raw_string(&input, "orchestrator")?.map(empty_string_to_none),
            context_files,
            upsert_artifacts: parse_artifacts(&input)?,
        },
        agent,
        model,
        owner.map(|owner| owner.owner_run_id),
    )?;
    serialize_task(runtime, &task)
}

/// Whether the caller explicitly opted out of the operator-surface check that
/// every `context_files` selector already exists. Internal callers never reach
/// these handlers, so the escape is the only way for an agent to record a
/// target the task is about to create.
fn allows_missing_context(input: &Value) -> Result<bool, OrbitError> {
    Ok(
        optional_bool_alias(input, &["allow_missing_context", "allowMissingContext"])?
            .unwrap_or(false),
    )
}

fn optional_raw_string_alias(input: &Value, keys: &[&str]) -> Result<Option<String>, OrbitError> {
    for key in keys {
        if let Some(value) = input.get(*key) {
            return match value {
                Value::Null => Ok(None),
                Value::String(raw) => Ok(Some(raw.to_string())),
                _ => Err(OrbitError::InvalidInput(format!(
                    "`{key}` must be a string"
                ))),
            };
        }
    }
    Ok(None)
}
