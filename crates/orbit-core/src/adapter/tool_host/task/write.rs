//! Handlers for the task write verbs, `add` and `update`, and for the
//! artifact put that shares the update write.
//!
//! The tool entry has already checked the input against the verb's spec, so
//! these read canonical field names only. A request the CLI built carries a
//! [`TASK_CLI_OPTIONS_PARAM`] object holding the CLI-only options; it selects
//! the operator command's semantics (its screening, `--approve`, `--force`),
//! which no MCP request can reach.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::governance::task::TASK_CLI_OPTIONS_PARAM;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_raw_string, optional_string,
    optional_string_list_alias, reject_unknown_tool_fields, required_string,
};
use orbit_types::task::{
    ExternalRef, TaskPriority, TaskStatus, is_task_show_projection_field,
    unknown_task_show_field_message,
};
use serde_json::{Map, Value};

use crate::OrbitRuntime;
use crate::application::task::{
    ContextCreationAuthorization, ScreenedTaskWrite, TaskAddParams, TaskUpdateAuthority,
    TaskUpdateParams, TaskWriteResponsePolicy, TaskWriteScreen, is_operator_rescue_close,
    screened_task_write_response_json,
};

use super::super::dispatch::ToolCaller;
use super::super::input::{
    empty_string_to_none, optional_bool_alias, parse_artifacts, parse_assessed_task_complexity,
    parse_optional_task_origin, parse_relations, parse_task_priority, parse_task_status,
    parse_task_type,
};

/// The CLI-only options `orbit task add` sends.
const ADD_CLI_OPTIONS: &[&str] = &[
    "parent_id",
    "dependencies",
    "plan",
    "external_refs",
    "status",
    "source_task_id",
];

/// The CLI-only options `orbit task update` sends.
const UPDATE_CLI_OPTIONS: &[&str] = &["approve", "force", "discard_candidate", "artifacts"];

/// The fields an approval write may carry besides its options: approval is
/// one transition the domain derives from the current status, so it takes no
/// edit and no explicit `status`.
const APPROVE_ALLOWED_FIELDS: &[&str] = &[
    "id",
    "note",
    "comment",
    "model",
    "workspace",
    "fields",
    "field",
    TASK_CLI_OPTIONS_PARAM,
];

/// The CLI-only options of a request the CLI built; `None` for any other
/// caller. The tool entry refuses the field from MCP.
fn cli_options<'a>(input: &'a Value, allowed: &[&str]) -> Result<Option<&'a Value>, OrbitError> {
    let Some(options) = input.get(TASK_CLI_OPTIONS_PARAM) else {
        return Ok(None);
    };
    if !options.is_object() {
        return Err(OrbitError::InvalidInput(format!(
            "`{TASK_CLI_OPTIONS_PARAM}` must be an object"
        )));
    }
    reject_unknown_tool_fields(options, allowed)?;
    Ok(Some(options))
}

/// The screen for a write: a CLI request keeps the operator command's binding
/// to the managed run that spawned this process, unless the tool host bound
/// the call to a reservation owner.
fn write_screen(
    cli: bool,
    allow_missing_context: bool,
    owner: Option<orbit_tools::ReservationOwnerContext>,
) -> TaskWriteScreen {
    match owner {
        Some(owner) => TaskWriteScreen::bound(allow_missing_context, Some(owner.owner_run_id)),
        None if cli => TaskWriteScreen::from_process(allow_missing_context),
        None => TaskWriteScreen::unbound(allow_missing_context),
    }
}

/// Create a task. One a claimed worker files carries, as its first comment,
/// the claim and run that filed it [ORB-14792].
pub(super) fn add(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    input: &Value,
) -> Result<TaskWriteOutput, OrbitError> {
    let cli = cli_options(input, ADD_CLI_OPTIONS)?;
    // The operator command passes its text as given, for the domain's own
    // checks, and its description defaults to empty; MCP callers must title
    // and describe the task.
    let (title, description) = match cli {
        Some(_) => (
            optional_raw_string(input, "title")?.unwrap_or_default(),
            optional_raw_string(input, "description")?.unwrap_or_default(),
        ),
        None => (
            required_string(input, &["title"], "title")?,
            required_string(input, &["description"], "description")?,
        ),
    };
    let response_fields = write_response_fields(input)?;
    // `workspace` is required for the existing MCP/CLI routing that selects
    // this runtime before dispatch reaches here; context selectors always
    // canonicalize against the repository root regardless of its value.
    let _ = required_string(input, &["workspace"], "workspace")?;
    let screen = write_screen(
        cli.is_some(),
        allows_missing_context(input)?,
        // Creation was never bound to a reservation owner.
        None,
    );
    let filed_by = caller.session_context.worker_invocation.as_ref();
    let empty = Value::Object(Map::new());
    let options = cli.unwrap_or(&empty);
    let written = runtime.add_task_screened(
        TaskAddParams {
            parent_id: optional_string(options, "parent_id")?,
            title,
            description,
            acceptance_criteria: optional_string_list_alias(input, &["acceptance_criteria"])?
                .unwrap_or_default(),
            dependencies: optional_csv_or_string_list_alias(options, &["dependencies"])?
                .unwrap_or_default(),
            relations: parse_relations(input)?.unwrap_or_default(),
            tags: optional_csv_or_string_list_alias(input, &["tags"])?.unwrap_or_default(),
            required_tools: optional_csv_or_string_list_alias(input, &["required_tools"])?
                .unwrap_or_default(),
            requires_labels: optional_csv_or_string_list_alias(input, &["requires_labels"])?
                .unwrap_or_default(),
            plan: optional_raw_string(options, "plan")?.unwrap_or_default(),
            crew_source: None,
            comment: filed_by.map(|binding| {
                format!(
                    "Filed by a claimed worker: claim {}, run {} on machine {}.",
                    binding.claim_id, binding.bound_run_id, binding.execution.machine_id
                )
            }),
            context_files: optional_csv_or_string_list_alias(input, &["context_files"])?
                .unwrap_or_default(),
            priority: optional_string(input, "priority")?
                .map(|value| parse_task_priority("priority", &value))
                .transpose()?
                .unwrap_or(TaskPriority::Medium),
            complexity: parse_assessed_task_complexity(
                "complexity",
                &required_string(input, &["complexity"], "complexity")?,
            )?,
            task_type: optional_string(input, "type")?
                .map(|value| parse_task_type("type", &value))
                .transpose()?,
            status: optional_string(options, "status")?
                .map(|value| creation_status(&value))
                .transpose()?,
            system_created: false,
            external_refs: optional_string_list_alias(options, &["external_refs"])?
                .unwrap_or_default()
                .iter()
                .map(|raw| ExternalRef::parse_key(raw))
                .collect::<Result<Vec<_>, _>>()?,
            source_task_id: optional_string(options, "source_task_id")?,
            crew: optional_string(input, "crew")?,
            orchestrator: optional_string(input, "orchestrator")?,
            orchestrator_model: optional_string(input, "orchestrator_model")?,
            origin: parse_optional_task_origin(input, "origin")?,
            context_creation: ContextCreationAuthorization::default(),
            creation_decision: None,
        },
        &screen,
        caller.agent,
        caller.model,
    )?;
    screened_write_response(runtime, written, response_fields.as_deref())
}

/// A status a task may be created in.
fn creation_status(value: &str) -> Result<TaskStatus, OrbitError> {
    let status = parse_task_status("status", value)?;
    match status {
        TaskStatus::Proposed | TaskStatus::Backlog | TaskStatus::Someday => Ok(status),
        _ => Err(OrbitError::InvalidInput(format!(
            "a task cannot be created in status '{status}'; use proposed, backlog or someday"
        ))),
    }
}

pub(super) fn update(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    input: &Value,
    operator: bool,
) -> Result<TaskWriteOutput, OrbitError> {
    if let Some(options) = cli_options(input, UPDATE_CLI_OPTIONS)? {
        return cli_update(runtime, caller, input, options);
    }
    let origin = runtime.artifact_origin(caller.session_context);
    let ToolCaller {
        agent,
        model,
        reservation_owner,
        ..
    } = caller;
    let id = required_string(input, &["id"], "id")?;
    let response_fields = write_response_fields(input)?;
    let requested_status = optional_string(input, "status")?
        .map(|value| parse_task_status("status", &value))
        .transpose()?;
    let screen = write_screen(false, allows_missing_context(input)?, reservation_owner);
    if let Some(target) = requested_status
        && matches!(target, TaskStatus::Backlog | TaskStatus::InProgress)
    {
        let current = runtime.get_task(&id)?;
        if let Some(kind) = guarded_lifecycle_write(current.status, target, input, operator)? {
            let written = match kind {
                GuardedLifecycleWrite::Approve => runtime.transition_task_to_backlog_screened(
                    &id,
                    optional_string(input, "note")?,
                    optional_string(input, "comment")?,
                    agent,
                    model,
                )?,
                GuardedLifecycleWrite::Start => {
                    let mut params = task_update_params_from_input(input, requested_status)?;
                    params.trusted_artifact_origin = origin.clone();
                    runtime.start_task_screened(
                        &id,
                        params,
                        &screen,
                        optional_string(input, "note")?,
                        agent,
                        model,
                    )?
                }
            };
            return screened_write_response(runtime, written, response_fields.as_deref());
        }
    }
    let note = optional_string(input, "note")?;
    if note.is_some() && requested_status.is_none() {
        return Err(OrbitError::InvalidInput(
            "`note` requires a status change; use `comment` for free-form discussion".to_string(),
        ));
    }
    let mut params = task_update_params_from_input(input, requested_status)?;
    params.trusted_artifact_origin = origin;
    let written =
        runtime.update_task_screened_for_tool(&id, params, &screen, agent, model, note)?;
    screened_write_response(runtime, written, response_fields.as_deref())
}

/// `orbit task update`: the operator command's write. `approve` takes the
/// task's next approval step, `force` applies a status the lifecycle refuses
/// (recorded as forced), and the status change runs as a plain lifecycle
/// update rather than the tool's guarded transitions.
fn cli_update(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    input: &Value,
    options: &Value,
) -> Result<TaskWriteOutput, OrbitError> {
    let ToolCaller {
        agent,
        model,
        reservation_owner,
        ..
    } = caller;
    let id = required_string(input, &["id"], "id")?;
    let response_fields = write_response_fields(input)?;
    let note = optional_string(input, "note")?;
    if optional_bool_alias(options, &["approve"])?.unwrap_or(false) {
        reject_fields_for_approve(input)?;
        let written = runtime.approve_task_screened(
            &id,
            note,
            optional_string(input, "comment")?,
            agent,
            model,
        )?;
        return screened_write_response(runtime, written, response_fields.as_deref());
    }
    if note.is_some() {
        return Err(OrbitError::InvalidInput(
            "`note` is recorded on an approval; pass it with `approve`".to_string(),
        ));
    }
    let status = optional_string(input, "status")?
        .map(|value| parse_task_status("status", &value))
        .transpose()?;
    let force = optional_bool_alias(options, &["force"])?.unwrap_or(false);
    if force && status.is_none() {
        return Err(OrbitError::InvalidInput(
            "`force` applies a status change; pass it with `status`".to_string(),
        ));
    }
    let mut params = task_update_params_from_input(input, status)?;
    params.upsert_artifacts = parse_artifacts(options)?;
    params.discard_candidate =
        optional_bool_alias(options, &["discard_candidate"])?.unwrap_or(false);
    let authority = if force {
        TaskUpdateAuthority::Forced
    } else {
        TaskUpdateAuthority::Lifecycle
    };
    let written = runtime.update_task_screened(
        &id,
        params,
        &write_screen(true, allows_missing_context(input)?, reservation_owner),
        authority,
        agent,
        model,
    )?;
    screened_write_response(runtime, written, response_fields.as_deref())
}

fn reject_fields_for_approve(input: &Value) -> Result<(), OrbitError> {
    let mut extras = update_object_fields(input)?
        .keys()
        .filter(|field| !APPROVE_ALLOWED_FIELDS.contains(&field.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(options) = input.get(TASK_CLI_OPTIONS_PARAM).and_then(Value::as_object) {
        extras.extend(
            options
                .keys()
                .filter(|option| option.as_str() != "approve")
                .cloned(),
        );
    }
    if extras.is_empty() {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`approve` takes the task's next approval step and cannot be combined with field edits or a status: {}",
        extras.join(", ")
    )))
}

/// `orbit.task.artifact.put`: store a prepared artifact payload through the
/// task update write.
pub(super) fn artifact_put(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    input: &Value,
    operator: bool,
) -> Result<TaskWriteOutput, OrbitError> {
    let origin = runtime.artifact_origin(caller.session_context);
    let id = required_string(input, &["id"], "id")?;
    let params = TaskUpdateParams {
        trusted_artifact_origin: origin,
        upsert_artifacts: parse_artifacts(input)?,
        ..TaskUpdateParams::default()
    };
    // Preserve the authenticated native operator decision, using the same
    // screened lifecycle write as `task update --artifact`. A transport or
    // worker call keeps the owner-aware tool path even on a human host.
    // The shared writer classifier still refuses agent identities/envelopes;
    // labels, omitted models and operator overrides cannot erase them.
    let native_operator = operator
        && !agent_context_declared()
        && caller.session_context.transport.is_none()
        && caller.session_context.worker_invocation.is_none();
    let screen = write_screen(native_operator, false, caller.reservation_owner);
    let written = if native_operator {
        runtime.update_task_screened(
            &id,
            params,
            &screen,
            TaskUpdateAuthority::Lifecycle,
            caller.agent,
            caller.model,
        )?
    } else {
        runtime.update_task_screened_for_tool(
            &id,
            params,
            &screen,
            caller.agent,
            caller.model,
            None,
        )?
    };
    screened_write_response(runtime, written, None)
}

/// The write response in the tool host's compact/projected profile:
/// the shared projection plus, only when non-empty, the write's
/// `warnings` and `context_files_unverified` selectors.
fn screened_write_response(
    runtime: &OrbitRuntime,
    written: ScreenedTaskWrite,
    fields: Option<&[String]>,
) -> Result<TaskWriteOutput, OrbitError> {
    let persisted_id = written.task.id.clone();
    let response = screened_task_write_response_json(
        runtime,
        &written,
        &TaskWriteResponsePolicy::from_fields(fields),
    )?;
    Ok(TaskWriteOutput {
        response,
        persisted_id,
    })
}

enum GuardedLifecycleWrite {
    Approve,
    Start,
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

fn update_object_fields(input: &Value) -> Result<&Map<String, Value>, OrbitError> {
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
        acceptance_criteria: optional_string_list_alias(input, &["acceptance_criteria"])?,
        dependencies: optional_csv_or_string_list_alias(input, &["dependencies"])?,
        relations: parse_relations(input)?,
        tags: optional_csv_or_string_list_alias(input, &["tags"])?,
        requires_labels: optional_csv_or_string_list_alias(input, &["requires_labels"])?,
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
        task_type: optional_string(input, "type")?
            .map(|value| parse_task_type("type", &value))
            .transpose()?,
        source_task_id: optional_raw_string(input, "source_task_id")?.map(empty_string_to_none),
        planned_by: optional_raw_string(input, "planned_by")?.map(empty_string_to_none),
        implemented_by: optional_raw_string(input, "implemented_by")?.map(empty_string_to_none),
        pr_status: optional_raw_string(input, "pr_status")?.map(empty_string_to_none),
        job_run_id: optional_raw_string(input, "job_run_id")?.map(empty_string_to_none),
        crew: optional_raw_string(input, "crew")?.map(empty_string_to_none),
        orchestrator: optional_raw_string(input, "orchestrator")?.map(empty_string_to_none),
        origin: parse_optional_task_origin(input, "origin")?,
        context_files: optional_csv_or_string_list_alias(input, &["context_files", "context"])?,
        // Derived from the core's selector screening, never from tool input.
        context_creation: ContextCreationAuthorization::default(),
        // Inline artifacts come only from the CLI's options and the artifact
        // put; the tool refuses an `artifacts` field.
        upsert_artifacts: Vec::new(),
        // Discarding a preserved candidate is an operator decision taken
        // from the CLI (`orbit task update --discard-candidate`).
        discard_candidate: false,
        allow_drop_system_tags: optional_bool_alias(input, &["allow_drop_system_tags"])?
            .unwrap_or(false),
    })
}

/// Whether the caller explicitly declared that missing `context_files`
/// selectors are targets the task will create. Internal callers never reach
/// these handlers, so the declaration is the only way for an agent to record
/// a target the task is about to create; it is recorded as durable creation
/// intent rather than skipping validation.
fn allows_missing_context(input: &Value) -> Result<bool, OrbitError> {
    Ok(optional_bool_alias(input, &["allow_missing_context"])?.unwrap_or(false))
}

/// The persisted identity travels beside the caller's response projection so
/// redaction audit attribution does not depend on which fields were requested.
pub(super) struct TaskWriteOutput {
    pub response: Value,
    pub persisted_id: String,
}

/// Read-only projection controls are accepted by mutating task tools solely
/// for shaping their response. In particular, `comments` and `history` are
/// omitted from the default write response and can be requested explicitly.
pub(super) fn write_response_fields(input: &Value) -> Result<Option<Vec<String>>, OrbitError> {
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
