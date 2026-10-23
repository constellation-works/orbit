//! Request shaping: the CLI takes one flag per field where the tool takes
//! structured `schedule`, `template`, `template_patch` and `waive_batch`
//! objects. Each CLI-only parameter is consumed here, so none reaches the tool.

use orbit_common::governance::auto_task::AutoTaskVerb;
use orbit_core::OrbitError;
use serde_json::{Map, Value, json};

/// The `list` state filter, applied to the tool's rows before rendering.
#[derive(Clone, Copy, Default)]
pub(super) struct StateFilter {
    pub enabled_only: bool,
    pub disabled_only: bool,
}

/// Shape an auto-task request for the CLI before it reaches the tool.
pub(super) fn prepare(
    verb: AutoTaskVerb,
    input: &mut Map<String, Value>,
) -> Result<StateFilter, OrbitError> {
    match verb {
        AutoTaskVerb::Add => {
            let schedule = take_schedule(input)?.ok_or_else(|| {
                OrbitError::InvalidInput(
                    "a schedule is required: pass one of `--cron`, `--every-minutes` or `--deliveries-landed`"
                        .to_string(),
                )
            })?;
            input.insert("schedule".to_string(), schedule);
            let mut template = Map::new();
            template.insert(
                "title".to_string(),
                take(input, "title").unwrap_or(json!("")),
            );
            template.insert(
                "description".to_string(),
                take(input, "body").unwrap_or(json!("")),
            );
            template.insert(
                "acceptance_criteria".to_string(),
                take(input, "criteria").unwrap_or(json!([])),
            );
            template.insert("tags".to_string(), take(input, "tags").unwrap_or(json!([])));
            template.insert(
                "required_tools".to_string(),
                take(input, "required_tools").unwrap_or(json!([])),
            );
            for (flag, field) in [
                ("task_type", "task_type"),
                ("priority", "priority"),
                ("complexity", "complexity"),
                ("status", "status"),
            ] {
                if let Some(value) = take(input, flag) {
                    template.insert(field.to_string(), snake_case(value));
                }
            }
            if let Some(crew) = take(input, "crew") {
                template.insert("crew".to_string(), crew);
            }
            input.insert("template".to_string(), Value::Object(template));
            normalize_dedupe(input);
            Ok(StateFilter::default())
        }
        AutoTaskVerb::Update => {
            if let Some(schedule) = take_schedule(input)? {
                input.insert("schedule".to_string(), schedule);
            }
            // Only named fields change; core merges them under the edit lock.
            let mut patch = Map::new();
            for (flag, field) in [
                ("title", "title"),
                ("body", "description"),
                ("criteria", "acceptance_criteria"),
                ("tags", "tags"),
                ("crew", "crew"),
            ] {
                if let Some(value) = take(input, flag) {
                    patch.insert(field.to_string(), value);
                }
            }
            for field in ["task_type", "priority", "complexity", "status"] {
                if let Some(value) = take(input, field) {
                    patch.insert(field.to_string(), snake_case(value));
                }
            }
            // One comma-separated value; an empty value clears the list.
            if let Some(Value::String(raw)) = take(input, "required_tools") {
                patch.insert(
                    "required_tools".to_string(),
                    json!(crate::parse::csv_to_vec(&raw)),
                );
            }
            if !patch.is_empty() {
                input.insert("template_patch".to_string(), Value::Object(patch));
            }
            match (take(input, "waive_batch_id"), take(input, "waiver_reason")) {
                (Some(batch_id), Some(reason)) => {
                    input.insert(
                        "waive_batch".to_string(),
                        json!({ "batch_id": batch_id, "reason": reason }),
                    );
                }
                (None, None) => {}
                _ => {
                    return Err(OrbitError::InvalidInput(
                        "`--waive-batch` and `--waiver-reason` must be passed together".to_string(),
                    ));
                }
            }
            normalize_dedupe(input);
            Ok(StateFilter::default())
        }
        AutoTaskVerb::List => Ok(StateFilter {
            enabled_only: take(input, "enabled").is_some(),
            disabled_only: take(input, "disabled").is_some(),
        }),
        AutoTaskVerb::Show
        | AutoTaskVerb::Toggle
        | AutoTaskVerb::Delete
        | AutoTaskVerb::Restore
        | AutoTaskVerb::Mint
        | AutoTaskVerb::Recover
        | AutoTaskVerb::Reset => Ok(StateFilter::default()),
    }
}

fn take(input: &mut Map<String, Value>, field: &str) -> Option<Value> {
    input.remove(field)
}

/// Compose `--cron`, `--every-minutes` and `--deliveries-landed` into one
/// schedule object. No flag means no schedule; more than one is refused.
fn take_schedule(input: &mut Map<String, Value>) -> Result<Option<Value>, OrbitError> {
    let cron = take(input, "cron");
    let every_minutes = take(input, "every_minutes");
    if let Some(Value::String(raw)) = take(input, "deliveries_landed") {
        if cron.is_some() || every_minutes.is_some() {
            return Err(OrbitError::InvalidInput(
                "delivery, cron and interval schedules are mutually exclusive".into(),
            ));
        }
        let trigger: Value = serde_json::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid deliveries-landed JSON: {error}"))
        })?;
        return Ok(Some(json!({ "deliveries_landed": trigger })));
    }
    match (cron, every_minutes) {
        (Some(_), Some(_)) => Err(OrbitError::InvalidInput(
            "specify exactly one of `--cron` and `--every-minutes`".to_string(),
        )),
        (Some(cron), None) => Ok(Some(json!({ "cron": cron }))),
        (None, Some(every_minutes)) => Ok(Some(json!({ "every_minutes": every_minutes }))),
        (None, None) => Ok(None),
    }
}

/// The CLI spells enum values in kebab-case; the wire takes snake_case.
fn snake_case(value: Value) -> Value {
    match value {
        Value::String(raw) => Value::String(raw.replace('-', "_")),
        other => other,
    }
}

fn normalize_dedupe(input: &mut Map<String, Value>) {
    if let Some(dedupe) = take(input, "dedupe") {
        input.insert("dedupe".to_string(), snake_case(dedupe));
    }
}
