//! MCP host functions for the `orbit.auto_task.*` tools [ORB-10149]. Thin
//! adapters that parse the JSON tool input into the shared CRUD params and
//! call the same `OrbitRuntime` methods the CLI uses, so both entry points
//! stay consistent. Each returns the full definition as JSON.

use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use orbit_automation::auto_tasks::crud::{AutoTaskAddParams, AutoTaskUpdateParams};

pub(super) fn add(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let name = required_str(&input, "name")?;
    let description = optional_str(&input, "description").unwrap_or_default();
    let schedule: AutoTaskSchedule = parse_field(&input, "schedule", true)?
        .ok_or_else(|| OrbitError::InvalidInput("missing `schedule`".to_string()))?;
    let template: AutoTaskTemplate = parse_field(&input, "template", true)?
        .ok_or_else(|| OrbitError::InvalidInput("missing `template`".to_string()))?;
    let dedupe: DedupePolicy = parse_field(&input, "dedupe", false)?.unwrap_or_default();

    let definition = runtime.auto_task_add(AutoTaskAddParams {
        name,
        description,
        schedule,
        template,
        dedupe,
    })?;
    let mut response = to_json(&definition)?;
    let warnings = runtime.validate_required_tools(&definition.template.required_tools)?;
    if !warnings.is_empty()
        && let Some(object) = response.as_object_mut()
    {
        object.insert("warnings".to_string(), json!(warnings));
    }
    Ok(response)
}

pub(super) fn list(runtime: &OrbitRuntime, _input: Value) -> Result<Value, OrbitError> {
    let definitions = runtime.auto_task_list()?;
    let array = definitions
        .iter()
        .map(|definition| serde_json::to_value(definition).unwrap_or(Value::Null))
        .collect();
    Ok(Value::Array(array))
}

/// Mint one task from a definition on demand [ORB-10798]. The adapter only
/// reads the name; `OrbitRuntime::auto_task_mint` owns the unconditional,
/// cursor-neutral behavior the CLI subcommand already relies on.
pub(super) fn mint(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let name = required_str(&input, "name")?;
    let task = runtime.auto_task_mint(&name)?;
    super::json::serialize_task(runtime, &task)
}

pub(super) fn show(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let name = required_str(&input, "name")?;
    let definition = runtime
        .auto_task_show(&name)?
        .ok_or_else(|| OrbitError::InvalidInput(format!("no such auto-task '{name}'")))?;
    let mut value = to_json(&definition)?;
    if matches!(definition.schedule, AutoTaskSchedule::Deliveries { .. }) {
        let diagnostic = if input
            .get("preview")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            orbit_automation::consumers::evaluate_auto_task(
                runtime,
                &definition,
                true,
                chrono::Utc::now(),
            )?
        } else {
            orbit_automation::consumers::inspect_auto_task(
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

pub(super) fn update(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let name = required_str(&input, "name")?;
    let params = AutoTaskUpdateParams {
        waive_batch: parse_field(&input, "waive_batch", false)?,
        description: optional_str(&input, "description"),
        schedule: parse_field(&input, "schedule", false)?,
        dedupe: parse_field(&input, "dedupe", false)?,
        template: parse_field(&input, "template", false)?,
    };
    let definition = runtime.auto_task_update(&name, params)?;
    let mut response = to_json(&definition)?;
    let warnings = runtime.validate_required_tools(&definition.template.required_tools)?;
    if !warnings.is_empty()
        && let Some(object) = response.as_object_mut()
    {
        object.insert("warnings".to_string(), json!(warnings));
    }
    Ok(response)
}

pub(super) fn toggle(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    let name = required_str(&input, "name")?;
    let enabled = input
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| OrbitError::InvalidInput("missing boolean `enabled`".to_string()))?;
    let definition = runtime.auto_task_toggle(&name, enabled)?;
    to_json(&definition)
}

fn required_str(input: &Value, field: &str) -> Result<String, OrbitError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing non-empty `{field}`")))
}

fn optional_str(input: &Value, field: &str) -> Option<String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// Deserialize a nested object field into a typed value. `required` only
/// affects the error when the field is present-but-malformed; an absent field
/// always yields `Ok(None)`.
fn parse_field<T: serde::de::DeserializeOwned>(
    input: &Value,
    field: &str,
    required: bool,
) -> Result<Option<T>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| {
                let context = if required { "required " } else { "" };
                OrbitError::InvalidInput(format!("invalid {context}`{field}`: {error}"))
            }),
    }
}

fn to_json(definition: &orbit_types::workflow::AutoTaskDefinition) -> Result<Value, OrbitError> {
    serde_json::to_value(definition)
        .map_err(|error| OrbitError::Io(format!("encode auto-task: {error}")))
}
