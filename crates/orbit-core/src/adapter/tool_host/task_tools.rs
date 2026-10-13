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
use crate::application::task::{
    ContextCreationAuthorization, ScreenedTaskWrite, TaskAddParams, TaskUpdateParams,
    TaskWriteScreen, is_operator_rescue_close,
};

use super::input::{
    empty_string_to_none, optional_bool_alias, parse_artifacts, parse_assessed_task_complexity,
    parse_relations, parse_task_priority, parse_task_status, parse_task_type,
};
use super::json::{serialize_task_artifact_read, serialize_task_write_response};

/// The persisted identity travels beside the caller's response projection so
/// redaction audit attribution does not depend on which fields were requested.
pub(super) struct TaskWriteOutput {
    pub response: Value,
    pub persisted_id: String,
}

/// Create a task. One a claimed worker files carries, as its first comment,
/// the claim and run that filed it [ORB-14792].
pub(super) fn add(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    filed_by: Option<&orbit_types::tool::WorkerInvocation>,
) -> Result<TaskWriteOutput, OrbitError> {
    let title = required_string(&input, &["title"], "title")?;
    let description = required_string(&input, &["description"], "description")?;
    let response_fields = write_response_fields(&input)?;
    // `workspace` is required for the existing MCP/CLI routing that selects
    // this runtime before dispatch reaches here; context selectors always
    // canonicalize against the repository root regardless of its value.
    let _ = required_string(&input, &["workspace"], "workspace")?;
    let context_files =
        optional_csv_or_string_list_alias(&input, &["context_files"])?.unwrap_or_default();
    let screen = TaskWriteScreen::unbound(allows_missing_context(&input)?);
    let required_tools = optional_csv_or_string_list_alias(
        &input,
        &["required_tools", "requiredTools", "required-tool"],
    )?
    .unwrap_or_default();
    let written = runtime.add_task_screened(
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
            required_tools,
            plan: String::new(),
            crew_source: None,
            comment: filed_by.map(|binding| {
                format!(
                    "Filed by a claimed worker: claim {}, run {} on machine {}.",
                    binding.claim_id, binding.bound_run_id, binding.execution.machine_id
                )
            }),
            context_files,
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
            context_creation: ContextCreationAuthorization::default(),
        },
        &screen,
        agent,
        model,
    )?;
    screened_write_response(runtime, written, response_fields.as_deref())
}

pub(super) fn delete(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let force = optional_bool_alias(&input, &["force"])?.unwrap_or(false);
    runtime.delete_task_guarded(&id, force)?;
    Ok(json!({ "id": id, "deleted": true }))
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
    let listing_hint = "list available artifacts with orbit.task.show and field: \"artifacts\"";
    let path = required_string(&input, &["path", "artifact_path", "artifactPath"], "path")
        .map_err(|error| OrbitError::InvalidInput(format!("{error}; {listing_hint}")))?;
    if path == "." {
        return Err(OrbitError::InvalidInput(format!(
            "`path` must name one artifact, not a directory; {listing_hint}"
        )));
    }
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
    operator: bool,
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
    let screen = TaskWriteScreen::bound(
        allows_missing_context(&input)?,
        owner.map(|owner| owner.owner_run_id),
    );
    if let Some(target) = requested_status
        && matches!(target, TaskStatus::Backlog | TaskStatus::InProgress)
    {
        let current = runtime.get_task(&id)?;
        if let Some(kind) = guarded_lifecycle_write(current.status, target, &input, operator)? {
            let written = match kind {
                GuardedLifecycleWrite::Approve => runtime.transition_task_to_backlog_screened(
                    &id,
                    optional_string(&input, "note")?,
                    optional_string(&input, "comment")?,
                    agent,
                    model,
                )?,
                GuardedLifecycleWrite::Start => {
                    let mut params = task_update_params_from_input(&input, requested_status)?;
                    params.trusted_artifact_origin = origin.clone();
                    runtime.start_task_screened(
                        &id,
                        params,
                        &screen,
                        optional_string(&input, "note")?,
                        agent,
                        model,
                    )?
                }
            };
            return screened_write_response(runtime, written, response_fields.as_deref());
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
    let written =
        runtime.update_task_screened_for_tool(&id, params, &screen, agent, model, note)?;
    screened_write_response(runtime, written, response_fields.as_deref())
}

/// The write response: the task projection plus, only when non-empty, the
/// write's `warnings` and the `context_files_unverified` selectors the
/// owning worker's relaxation stored, so a strict write's response is the
/// bare projection.
fn screened_write_response(
    runtime: &OrbitRuntime,
    written: ScreenedTaskWrite,
    fields: Option<&[String]>,
) -> Result<TaskWriteOutput, OrbitError> {
    let ScreenedTaskWrite {
        task,
        warnings,
        context_files_unverified,
    } = written;
    let mut response = serialize_task_write_response(runtime, &task, fields)?;
    if let Some(obj) = response.as_object_mut() {
        if !context_files_unverified.is_empty() {
            obj.insert(
                "context_files_unverified".to_string(),
                json!(context_files_unverified),
            );
        }
        if !warnings.is_empty() {
            obj.insert("warnings".to_string(), json!(warnings));
        }
    }
    Ok(TaskWriteOutput {
        response,
        persisted_id: task.id,
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
/// Field edits on a start write are absorbed by the start body. The one
/// exception is an operator's rescue close of a `blocked` task, which starts
/// no work and takes the ordinary governed update. Any other `backlog`
/// combination — including `someday → backlog` plus a field edit — falls
/// through to the ordinary governed update.
fn guarded_lifecycle_write(
    from: TaskStatus,
    to: TaskStatus,
    input: &Value,
    operator: bool,
) -> Result<Option<GuardedLifecycleWrite>, OrbitError> {
    match (from, to) {
        (TaskStatus::Proposed, TaskStatus::Backlog) => {
            reject_fields_for_approval_transition(input)?;
            Ok(Some(GuardedLifecycleWrite::Approve))
        }
        (_, TaskStatus::InProgress)
            if is_operator_rescue_close(
                from,
                to,
                optional_raw_string(input, "execution_summary")?.as_deref(),
                operator,
            ) =>
        {
            Ok(None)
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
        crew: match input.get("crew") {
            Some(Value::Null) => {
                return Err(OrbitError::InvalidInput(
                    "`crew` must be a string; omit it to keep the current crew or use an empty string to redraw"
                        .to_string(),
                ));
            }
            _ => optional_raw_string(input, "crew")?.map(empty_string_to_none),
        },
        orchestrator: optional_raw_string(input, "orchestrator")?.map(empty_string_to_none),
        context_files: optional_csv_or_string_list_alias(input, &["context_files", "context"])?,
        // Derived from the core's selector screening, never from tool input.
        context_creation: ContextCreationAuthorization::default(),
        upsert_artifacts: parse_artifacts(input)?,
        // Discarding a preserved candidate is an operator decision taken
        // from the CLI (`orbit task update --discard-candidate`).
        discard_candidate: false,
        allow_drop_system_tags: optional_bool_alias(
            input,
            &[
                "allow_drop_system_tags",
                "allowDropSystemTags",
                "allow_drop_system_tag",
                "allow_dropping_system_tags",
            ],
        )?
        .unwrap_or(false),
    })
}

/// Whether the caller explicitly declared that missing `context_files`
/// selectors are targets the task will create. Internal callers never reach
/// these handlers, so the declaration is the only way for an agent to record
/// a target the task is about to create; it is recorded as durable creation
/// intent rather than skipping validation.
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
