//! `add`: append a friction report.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::governance::friction::{normalize_friction_tag_aliases, normalize_title};
use orbit_common::governance::operation::OperationInput;
use orbit_store::contract::FrictionAddParams;
use serde_json::Value;

use super::record::record_to_json_with_tag_normalizations;
use crate::OrbitRuntime;

pub(super) fn add(
    runtime: &OrbitRuntime,
    input: &OperationInput,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    // Ordinary MCP sessions (including federated destinations) have no
    // execution model. Match task writes by using the receiving process's
    // resolved actor instead of requiring a caller to supply provenance.
    let model = model.unwrap_or_else(|| runtime.actor().label.clone());
    let (params, substitutions) = add_params(input, Some(model))?;
    if let Some(task_id) = params.during_task.as_deref() {
        runtime.ensure_friction_task_exists(task_id)?;
    }
    let stored = crate::runtime::stored::friction::store_for(runtime)?.add(params)?;
    record_to_json_with_tag_normalizations(stored, substitutions)
}

pub(crate) fn add_params(
    input: &OperationInput,
    model: Option<String>,
) -> Result<(FrictionAddParams, Vec<(String, String)>), OrbitError> {
    let body = input.required_string("body")?;
    let title = input
        .optional_raw_string("title")?
        .map(|raw| normalize_title(&raw))
        .transpose()?;
    let tags = input.optional_string_list("tags")?.unwrap_or_default();
    let (tags, substitutions) = normalize_friction_tag_aliases(tags);
    let during_task = input.optional_string("during_task")?;
    let model = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "orbit.friction action add needs a session model (crew config, or --model on the CLI)"
                    .to_string(),
            )
        })?;
    Ok((
        FrictionAddParams {
            model,
            title,
            body,
            tags,
            status: orbit_types::record::FrictionStatus::Open,
            during_task,
            created_at: Utc::now(),
        },
        substitutions,
    ))
}
