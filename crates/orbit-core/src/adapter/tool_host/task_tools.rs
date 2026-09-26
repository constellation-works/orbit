use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string, optional_string_alias,
    optional_string_list_alias, required_string,
};
use orbit_types::task::{
    TaskPriority, TaskStatus, is_task_show_projection_field, unknown_task_show_field_message,
    validate_relative_artifact_path,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams, compute_task_add_warnings};

use super::input::{
    empty_string_to_none, optional_bool_alias, parse_artifacts, parse_assessed_task_complexity,
    parse_relations, parse_task_priority, parse_task_status, parse_task_type,
};
use super::json::{
    serialize_task, serialize_task_artifact_read, serialize_task_lint_report,
    serialize_task_write_response, task_fields_to_json, task_to_json,
};

/// The persisted identity travels beside the caller's response projection so
/// redaction audit attribution does not depend on which fields were requested.
pub(super) struct TaskWriteOutput {
    pub response: Value,
    pub persisted_id: String,
}

pub(super) fn add(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<TaskWriteOutput, OrbitError> {
    let title = required_string(&input, &["title"], "title")?;
    let description = required_string(&input, &["description"], "description")?;
    let response_fields = write_response_fields(&input)?;
    // `workspace` is required for the existing MCP/CLI routing that selects
    // this runtime before dispatch reaches here; context selectors always
    // canonicalize against the repository root regardless of its value.
    let _ = required_string(&input, &["workspace"], "workspace")?;
    let raw_context_files =
        optional_csv_or_string_list_alias(&input, &["context_files"])?.unwrap_or_default();
    if !allows_missing_context(&input)? {
        runtime.ensure_context_selectors_exist(&raw_context_files)?;
    }
    let raw_required_tools = optional_csv_or_string_list_alias(
        &input,
        &["required_tools", "requiredTools", "required-tool"],
    )?
    .unwrap_or_default();
    let mut warnings = runtime.validate_required_tools(&raw_required_tools)?;
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
            required_tools: raw_required_tools,
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
    let mut response = serialize_task_write_response(runtime, &task, response_fields.as_deref())?;
    warnings.extend(compute_task_add_warnings(
        &raw_context_files,
        task.task_type,
    ));
    if !warnings.is_empty()
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert("warnings".to_string(), json!(warnings));
    }
    Ok(TaskWriteOutput {
        response,
        persisted_id: task.id,
    })
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
) -> Result<TaskWriteOutput, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let note = required_string(&input, &["note"], "note")?;
    let response_fields = write_response_fields(&input)?;
    let task = runtime.reject_task_with_identity(
        &id,
        note,
        optional_string(&input, "comment")?,
        agent,
        model,
    )?;
    Ok(TaskWriteOutput {
        response: serialize_task_write_response(runtime, &task, response_fields.as_deref())?,
        persisted_id: task.id,
    })
}

pub(super) fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let task = runtime.get_task(&id)?;
    let fields = optional_csv_or_string_list_alias(&input, &["fields", "field"])?;
    let value = if let Some(fields) = &fields {
        let projected = task_fields_to_json(runtime, &task, fields)?;
        if fields.len() == 1 && fields[0] == "terminal" {
            json!({ fields[0].clone(): projected })
        } else {
            projected
        }
    } else {
        serialize_task(runtime, &task)?
    };
    Ok(value)
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

pub(super) fn update(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    owner: Option<orbit_tools::ReservationOwnerContext>,
    origin: Option<orbit_types::task::ExecutionLocation>,
) -> Result<TaskWriteOutput, OrbitError> {
    if ["required_tools", "requiredTools", "required-tool"]
        .iter()
        .any(|field| input.get(*field).is_some())
    {
        return Err(OrbitError::InvalidInput(
            "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation"
                .to_string(),
        ));
    }
    if input.get("force").is_some() {
        return Err(OrbitError::InvalidInput(
            "orbit.task.update does not accept `force`; lifecycle transitions are enforced for agents, and the override is a human CLI action"
                .to_string(),
        ));
    }
    let id = required_string(&input, &["id"], "id")?;
    let response_fields = write_response_fields(&input)?;
    let requested_status = optional_string(&input, "status")?
        .map(|value| parse_task_status("status", &value))
        .transpose()?;
    if let Some(target) = requested_status
        && matches!(target, TaskStatus::Backlog | TaskStatus::InProgress)
    {
        let current = runtime.get_task(&id)?;
        if let Some(kind) = guarded_lifecycle_write(current.status, target, &input)? {
            let (task, unverified) = match kind {
                GuardedLifecycleWrite::Approve => (
                    runtime.transition_task_to_backlog_with_identity(
                        &id,
                        optional_string(&input, "note")?,
                        optional_string(&input, "comment")?,
                        agent,
                        model,
                    )?,
                    Vec::new(),
                ),
                GuardedLifecycleWrite::Start => {
                    let mut params = task_update_params_from_input(&input, requested_status)?;
                    params.trusted_artifact_origin = origin.clone();
                    let unverified = ensure_context_selectors_if_required(
                        runtime,
                        &id,
                        &input,
                        &params,
                        owner.as_ref(),
                    )?;
                    let task = runtime.start_task_with_identity_and_crew(
                        &id,
                        optional_string(&input, "note")?,
                        params.comment.clone(),
                        agent,
                        model,
                        params.crew.clone().flatten(),
                        params.plan.clone(),
                        params,
                        owner.map(|owner| owner.owner_run_id),
                    )?;
                    (task, unverified)
                }
            };
            return write_response_with_unverified_context(
                runtime,
                &task,
                response_fields.as_deref(),
                unverified,
            );
        }
    }
    let note = optional_string(&input, "note")?;
    if note.is_some() && requested_status.is_none() {
        return Err(OrbitError::InvalidInput(
            "`note` requires a status change; use `comment` for free-form discussion".to_string(),
        ));
    }
    let mut params = task_update_params_from_input(&input, requested_status)?;
    params.trusted_artifact_origin = origin.clone();
    let unverified =
        ensure_context_selectors_if_required(runtime, &id, &input, &params, owner.as_ref())?;
    let task = runtime.update_task_with_owner(
        &id,
        params,
        agent,
        model,
        note,
        owner.map(|owner| owner.owner_run_id),
    )?;
    write_response_with_unverified_context(runtime, &task, response_fields.as_deref(), unverified)
}

/// Response key listing the canonical selectors this write stored without a
/// verified anchor because the owning worker declared them from its worktree.
/// Present only when the relaxation actually applied, so the response stays
/// unchanged for every strict write.
const CONTEXT_FILES_UNVERIFIED_KEY: &str = "context_files_unverified";

fn write_response_with_unverified_context(
    runtime: &OrbitRuntime,
    task: &orbit_types::task::Task,
    fields: Option<&[String]>,
    unverified: Vec<String>,
) -> Result<TaskWriteOutput, OrbitError> {
    let mut response = serialize_task_write_response(runtime, task, fields)?;
    if !unverified.is_empty()
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert(CONTEXT_FILES_UNVERIFIED_KEY.to_string(), json!(unverified));
    }
    Ok(TaskWriteOutput {
        response,
        persisted_id: task.id.clone(),
    })
}

enum GuardedLifecycleWrite {
    Approve,
    Start,
}

/// Read-only projection controls are accepted by mutating task tools solely
/// for shaping their response. In particular, `comments` and `history` are
/// omitted from the default write response and can be requested explicitly.
fn write_response_fields(input: &Value) -> Result<Option<Vec<String>>, OrbitError> {
    let fields = optional_csv_or_string_list_alias(input, &["fields", "field"])?;
    if let Some(fields) = &fields
        && let Some(unknown) = fields
            .iter()
            .find(|field| !is_task_show_projection_field(field))
    {
        return Err(OrbitError::InvalidInput(unknown_task_show_field_message(
            unknown,
        )));
    }
    Ok(fields)
}

const APPROVAL_ALLOWED_FIELDS: &[&str] = &[
    "id",
    "status",
    "note",
    "comment",
    "model",
    "workspace",
    "fields",
    "field",
];

/// Choose the special transition body only when this write actually needs it.
///
/// `proposed → backlog` is approval. Every `in-progress` write goes through
/// `start_task` so crew resolution and `TaskStarted` survive, and so a
/// non-pickup source is refused the same way with or without extra fields.
/// Field edits on a start write are absorbed by the start body. Any other
/// `backlog` combination — including `someday → backlog` plus a field edit —
/// falls through to the ordinary governed update.
fn guarded_lifecycle_write(
    from: TaskStatus,
    to: TaskStatus,
    input: &Value,
) -> Result<Option<GuardedLifecycleWrite>, OrbitError> {
    match (from, to) {
        (TaskStatus::Proposed, TaskStatus::Backlog) => {
            reject_fields_for_approval_transition(input)?;
            Ok(Some(GuardedLifecycleWrite::Approve))
        }
        (_, TaskStatus::InProgress) => Ok(Some(GuardedLifecycleWrite::Start)),
        _ => Ok(None),
    }
}

fn reject_fields_for_approval_transition(input: &Value) -> Result<(), OrbitError> {
    let fields = update_object_fields(input)?;
    let extras = fields
        .keys()
        .filter(|field| !APPROVAL_ALLOWED_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if extras.is_empty() {
        return Ok(());
    }

    Err(OrbitError::InvalidInput(format!(
        "status 'backlog' on a proposed task runs the guarded approval transition and cannot be combined with field edits: {}",
        extras.join(", ")
    )))
}

fn update_object_fields(input: &Value) -> Result<&serde_json::Map<String, Value>, OrbitError> {
    input.as_object().ok_or_else(|| {
        OrbitError::InvalidInput("orbit.task.update input must be an object".to_string())
    })
}

fn optional_plan(input: &Value) -> Result<Option<String>, OrbitError> {
    match input.get("plan") {
        None => Ok(None),
        Some(Value::String(raw)) => Ok(Some(raw.to_string())),
        Some(_) => Err(OrbitError::InvalidInput(
            "`plan` must be a string".to_string(),
        )),
    }
}

fn task_update_params_from_input(
    input: &Value,
    status: Option<TaskStatus>,
) -> Result<TaskUpdateParams, OrbitError> {
    Ok(TaskUpdateParams {
        trusted_artifact_origin: None,
        title: optional_string(input, "title")?,
        description: input
            .get("description")
            .map(|value| {
                value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                    OrbitError::InvalidInput("`description` must be a string".to_string())
                })
            })
            .transpose()?,
        acceptance_criteria: optional_string_list_alias(
            input,
            &[
                "acceptance_criteria",
                "acceptanceCriteria",
                "acceptance-criteria",
            ],
        )?,
        dependencies: optional_csv_or_string_list_alias(input, &["dependencies"])?,
        relations: parse_relations(input)?,
        tags: optional_csv_or_string_list_alias(input, &["tags", "tag"])?,
        plan: optional_plan(input)?,
        execution_summary: optional_raw_string(input, "execution_summary")?,
        comment: optional_string(input, "comment")?,
        status,
        priority: optional_string(input, "priority")?
            .map(|value| parse_task_priority("priority", &value))
            .transpose()?,
        complexity: optional_string(input, "complexity")?
            .map(|value| parse_assessed_task_complexity("complexity", &value))
            .transpose()?,
        task_type: optional_string_alias(input, &["type", "task_type", "taskType"])?
            .map(|value| parse_task_type("type", &value))
            .transpose()?,
        source_task_id: optional_raw_string_alias(
            input,
            &["source_task_id", "source_task", "sourceTaskId"],
        )?
        .map(empty_string_to_none),
        planned_by: optional_raw_string(input, "planned_by")?.map(empty_string_to_none),
        implemented_by: optional_raw_string(input, "implemented_by")?.map(empty_string_to_none),
        pr_status: optional_raw_string(input, "pr_status")?.map(empty_string_to_none),
        job_run_id: optional_raw_string(input, "job_run_id")?.map(empty_string_to_none),
        crew: optional_raw_string(input, "crew")?.map(empty_string_to_none),
        orchestrator: optional_raw_string(input, "orchestrator")?.map(empty_string_to_none),
        context_files: optional_csv_or_string_list_alias(input, &["context_files", "context"])?,
        upsert_artifacts: parse_artifacts(input)?,
    })
}

/// Run the operator-surface selector guard for an update unless the caller
/// opted out, and return the selectors the owning worker's relaxation let
/// through unverified (see
/// [`OrbitRuntime::ensure_context_selectors_exist_for_task_write`]).
fn ensure_context_selectors_if_required(
    runtime: &OrbitRuntime,
    task_id: &str,
    input: &Value,
    params: &TaskUpdateParams,
    owner: Option<&orbit_tools::ReservationOwnerContext>,
) -> Result<Vec<String>, OrbitError> {
    if allows_missing_context(input)? {
        return Ok(Vec::new());
    }
    let Some(candidates) = params.context_files.as_deref() else {
        return Ok(Vec::new());
    };
    runtime.ensure_context_selectors_exist_for_task_write(
        task_id,
        owner.map(|owner| owner.owner_run_id.as_str()),
        candidates,
    )
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
