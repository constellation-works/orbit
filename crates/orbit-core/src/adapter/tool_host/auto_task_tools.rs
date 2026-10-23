//! The auto-task handler table.
//!
//! Auto-task verbs are declared once as data in
//! `orbit_common::governance::auto_task::operations`. Handlers need
//! `&OrbitRuntime`, which lives above that leaf crate, so the handler half of
//! the table lives here and is joined to the spec half by [`AutoTaskVerb`].
//! [`dispatch`] is the join point, and its exhaustive `match` is what makes a
//! spec without a handler a compile error.
//!
//! Every surface reaches these handlers through the tool: MCP, `orbit tool
//! run`, and the `orbit auto-task` CLI, whose flags the CLI composes into the
//! structured fields read here. The tool entry checked the input against the
//! verb's spec, so handlers read canonical names and keep only content rules.

use orbit_common::OrbitError;
use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::automation::WaiveBatchRequest;
use orbit_types::workflow::automation::recovery::{RecoveryRequest, ResetRequest};
use orbit_types::workflow::{AutoTaskDefinition, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::auto_tasks::{
    AutoTaskAddParams, AutoTaskDeleteParams, AutoTaskTemplatePatch, AutoTaskUpdateParams,
    ListedAutoTask, cursor_state_path, load_cursor_state,
};

/// Run one auto-task verb.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AutoTaskVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    // Re-wraps the input the tool entry already parsed; see `parse_input`.
    let input = verb.spec().parse_input(input)?;
    match verb {
        AutoTaskVerb::Add => add(runtime, &input),
        AutoTaskVerb::List => list(runtime, &input),
        AutoTaskVerb::Show => show(runtime, &input),
        AutoTaskVerb::Update => update(runtime, &input),
        AutoTaskVerb::Toggle => toggle(runtime, &input),
        AutoTaskVerb::Delete => delete(runtime, &input),
        AutoTaskVerb::Restore => {
            to_json(&runtime.auto_task_restore(&input.required_string("name")?)?)
        }
        AutoTaskVerb::Mint => mint(runtime, &input),
        AutoTaskVerb::Recover => recover(runtime, &input),
        AutoTaskVerb::Reset => reset(runtime, &input),
    }
}

fn add(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    let description = input
        .optional_raw_string("description")?
        .unwrap_or_default();
    let schedule: AutoTaskSchedule = parse_field(input, "schedule", true)?
        .ok_or_else(|| OrbitError::InvalidInput("missing `schedule`".to_string()))?;
    let template: AutoTaskTemplate = parse_field(input, "template", true)?
        .ok_or_else(|| OrbitError::InvalidInput("missing `template`".to_string()))?;
    let dedupe: DedupePolicy = parse_field(input, "dedupe", false)?.unwrap_or_default();

    // Validate before the write, so an ungrantable tool leaves nothing behind.
    let warnings = runtime.validate_required_tools(&template.required_tools)?;
    let definition = runtime.auto_task_add(AutoTaskAddParams {
        name,
        description,
        schedule,
        template,
        dedupe,
    })?;
    with_warnings(to_json(&definition)?, warnings)
}

/// Definitions whose seeding plugin is off in this workspace are omitted
/// unless `include_inactive_plugins` is true; then each is marked with the
/// plugin and the reason it never fires. A delivery definition this host can
/// never admit carries that reason too.
fn list(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let include_inactive_plugins = input
        .optional_bool("include_inactive_plugins")?
        .unwrap_or(false);
    let array = runtime
        .auto_task_listing_state(include_inactive_plugins, chrono::Utc::now())?
        .rows
        .iter()
        .map(|row| {
            listed_json(
                &row.listed,
                row.effective_enabled,
                row.skipped_reason.as_deref(),
            )
        })
        .collect::<Result<_, _>>()?;
    Ok(Value::Array(array))
}

/// The canonical definition record, plus the inactive-plugin marker when its
/// seeding plugin is off here and the reason when it never fires here. A
/// live definition is the bare record.
fn listed_json(
    listed: &ListedAutoTask,
    effective_enabled: bool,
    skipped_reason: Option<&str>,
) -> Result<Value, OrbitError> {
    let mut value = to_json(&listed.definition)?;
    value["effective_enabled"] = json!(effective_enabled);
    let Some(object) = value.as_object_mut() else {
        return Ok(value);
    };
    if let Some(inactive) = &listed.inactive_plugin {
        object.insert("plugin_inactive".to_string(), json!(true));
        object.insert("inactive_plugin".to_string(), json!(inactive));
    }
    if let Some(reason) = skipped_reason {
        object.insert("skipped_reason".to_string(), json!(reason));
    }
    Ok(value)
}

/// One definition with the diagnostics that explain why it did or did not
/// mint: where it is defined, how its settings layer over a bundled body, the
/// host-local scheduler cursor, and a delivery definition's automation state.
fn show(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    let definition = existing_definition(runtime, &name)?;
    // A definition listings hide still resolves here, marked inactive.
    let listed = runtime.listed_auto_task(definition);
    let mut value = listed_json(
        &listed,
        runtime.auto_task_enabled(&listed.definition),
        listed.skipped_reason.as_deref(),
    )?;
    let (definition_root, definition_path) = runtime
        .auto_task_read_source(&name)?
        .ok_or_else(|| runtime.auto_task_missing(&name, true))?;
    value["definition_source"] = json!({
        "root": definition_root,
        "path": definition_path,
    });
    match runtime.auto_task_layering(&name) {
        Ok(layering) => value["layering"] = json!(layering),
        Err(error) => value["layering_error"] = json!(error.to_string()),
    }
    // An unreadable cursor file is reported, never rendered as "never
    // observed".
    match load_cursor_state(&cursor_state_path(&runtime.paths().state_dir)) {
        Ok(state) => {
            value["cursor"] = match state.definitions.get(&name) {
                Some(cursor) => serde_json::to_value(cursor)
                    .map_err(|error| OrbitError::Io(format!("encode cursor: {error}")))?,
                None => Value::Null,
            };
        }
        Err(error) => value["cursor_state_error"] = json!(error.to_string()),
    }
    let definition = listed.definition;
    if let Some(note) =
        crate::application::automation::review_crew_overrides_definition(runtime, &definition)
    {
        value["crew_overridden"] = json!(note);
    }
    if matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. }) {
        let diagnostic = if input.optional_bool("preview")?.unwrap_or(false) {
            crate::application::automation::evaluate_auto_task(
                runtime,
                &definition,
                true,
                chrono::Utc::now(),
            )?
        } else {
            crate::application::automation::inspect_auto_task(
                runtime,
                &definition,
                chrono::Utc::now(),
            )?
        };
        value["automation"] = serde_json::to_value(&diagnostic)
            .map_err(|e| OrbitError::InvalidInput(e.to_string()))?;
    }
    Ok(value)
}

fn update(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    // The tool's `template` is a full replacement, including defaults for
    // omitted fields. `template_patch`, which the CLI composes from its flags,
    // changes only the fields it names, merged under the edit lock.
    let template = match (
        parse_field::<AutoTaskTemplate>(input, "template", false)?,
        parse_field::<TemplatePatchInput>(input, "template_patch", false)?,
    ) {
        (Some(_), Some(_)) => {
            return Err(OrbitError::InvalidInput(
                "`template` and `template_patch` are mutually exclusive".to_string(),
            ));
        }
        (Some(template), None) => Some(template.into()),
        (None, Some(patch)) => Some(patch.into()),
        (None, None) => None,
    };
    let params = AutoTaskUpdateParams {
        waive_batch: parse_field::<WaiveBatchRequest>(input, "waive_batch", false)?,
        description: input.optional_raw_string("description")?,
        schedule: parse_field(input, "schedule", false)?,
        dedupe: parse_field(input, "dedupe", false)?,
        template,
        enabled: input.optional_bool("enabled")?,
    };
    let waived = params.waive_batch.is_some();
    let definition = runtime.auto_task_update(&name, params)?;
    let mut response = to_json(&definition)?;
    response["effective_enabled"] = json!(runtime.auto_task_enabled(&definition));
    let mut warnings = runtime.auto_task_update_tool_warnings(&definition, waived)?;
    // An edited crew the review pool replaces would otherwise read as applied.
    warnings.extend(
        crate::application::automation::review_crew_overrides_definition(runtime, &definition),
    );
    with_warnings(response, warnings)
}

/// A field-scoped template edit: absent fields are unchanged and present
/// lists replace the stored ones.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplatePatchInput {
    title: Option<String>,
    description: Option<String>,
    acceptance_criteria: Option<Vec<String>>,
    task_type: Option<TaskType>,
    tags: Option<Vec<String>>,
    required_tools: Option<Vec<String>>,
    priority: Option<TaskPriority>,
    complexity: Option<TaskComplexity>,
    crew: Option<String>,
    status: Option<TaskStatus>,
}

impl From<TemplatePatchInput> for AutoTaskTemplatePatch {
    fn from(patch: TemplatePatchInput) -> Self {
        AutoTaskTemplatePatch {
            title: patch.title,
            description: patch.description,
            acceptance_criteria: patch.acceptance_criteria,
            task_type: patch.task_type,
            tags: patch.tags,
            required_tools: patch.required_tools,
            context_files: None,
            priority: patch.priority,
            complexity: patch.complexity.map(Some),
            crew: patch.crew.map(Some),
            status: patch.status,
        }
    }
}

fn toggle(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    let enabled = match input.required_string("state")?.as_str() {
        "on" => true,
        "off" => false,
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "`state` must be `on` or `off`, not `{other}`"
            )));
        }
    };
    to_json(&runtime.auto_task_toggle(&name, enabled)?)
}

fn delete(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let report = runtime.auto_task_delete(AutoTaskDeleteParams {
        name: input.required_string("name")?,
        reason: input.optional_string("reason")?,
        force: input.optional_bool("force")?.unwrap_or(false),
    })?;
    serde_json::to_value(&report)
        .map_err(|error| OrbitError::Io(format!("encode auto-task delete report: {error}")))
}

/// Mint one task from a definition on demand. `OrbitRuntime::auto_task_mint`
/// owns the unconditional, cursor-neutral behavior.
fn mint(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let task = runtime.auto_task_mint(&input.required_string("name")?)?;
    super::json::serialize_task(runtime, &task)
}

/// Recover a delivery consumer stalled by settings or rebased history.
/// Without an applying request this is a read-only preview.
fn recover(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    let request = RecoveryRequest {
        adopt_settings: input.optional_bool("adopt_settings")?.unwrap_or(false),
        reissue_action: input.optional_bool("reissue_action")?.unwrap_or(false),
        replay_history: input.optional_bool("replay_history")?.unwrap_or(false),
        reason: input.optional_string("reason")?.unwrap_or_default(),
    };
    if request.replay_history && (request.adopt_settings || request.reissue_action) {
        return Err(OrbitError::InvalidInput(
            "`replay_history` cannot be combined with `adopt_settings` or `reissue_action`"
                .to_string(),
        ));
    }
    let definition = if request.mutates() {
        runtime.require_local_auto_task(&name)?
    } else {
        existing_definition(runtime, &name)?
    };
    let preview = crate::application::automation::recover_auto_task(
        runtime,
        &definition,
        &request,
        chrono::Utc::now(),
    )?;
    serde_json::to_value(&preview).map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

/// Forget one delivery consumer's coverage debt and re-baseline it. Without a
/// `reason` this only previews what would be forgotten.
fn reset(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let name = input.required_string("name")?;
    let request = ResetRequest {
        reason: input.optional_string("reason")?.unwrap_or_default(),
        force: input.optional_bool("force")?.unwrap_or(false),
    };
    let definition = if request.mutates() {
        runtime.require_local_auto_task(&name)?
    } else {
        existing_definition(runtime, &name)?
    };
    let preview = crate::application::automation::reset_auto_task(
        runtime,
        &definition,
        &request,
        chrono::Utc::now(),
    )?;
    serde_json::to_value(&preview).map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

fn existing_definition(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<AutoTaskDefinition, OrbitError> {
    runtime
        .auto_task_show(name)?
        .ok_or_else(|| runtime.auto_task_missing(name, true))
}

fn with_warnings(mut response: Value, warnings: Vec<String>) -> Result<Value, OrbitError> {
    if !warnings.is_empty()
        && let Some(object) = response.as_object_mut()
    {
        object.insert("warnings".to_string(), json!(warnings));
    }
    Ok(response)
}

/// Deserialize a structured field into a typed value. `required` only
/// affects the error when the field is present-but-malformed; an absent field
/// always yields `Ok(None)`.
fn parse_field<T: serde::de::DeserializeOwned>(
    input: &OperationInput,
    field: &str,
    required: bool,
) -> Result<Option<T>, OrbitError> {
    match input.as_value().get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                let context = if required { "required " } else { "" };
                OrbitError::InvalidInput(format!("invalid {context}`{field}`: {error}"))
            }),
    }
}

fn to_json(definition: &AutoTaskDefinition) -> Result<Value, OrbitError> {
    serde_json::to_value(definition)
        .map_err(|error| OrbitError::Io(format!("encode auto-task: {error}")))
}
