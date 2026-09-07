use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string, required_string};
use orbit_tools::ReservationOwnerContext;
use orbit_types::identity::normalize_optional_attribution_label;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::pipeline::{ChildPipelineAdmission, ChildSubmission};

use super::input::{
    parse_optional_poll_interval_seconds, parse_optional_timeout_seconds, parse_string_array_field,
    parse_task_priority, require_object_field,
};
use super::json::serialize_error;

pub(super) fn invoke(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
) -> Result<Value, OrbitError> {
    let job_name = required_string(&input, &["job_name"], "job_name")?;
    let payload = require_object_field(&input, "input")?.clone();
    let priority = optional_string(&input, "priority")?
        .map(|value| parse_task_priority("priority", &value))
        .transpose()?
        .map(|value| value.to_string());
    let actor = Some(
        normalize_optional_attribution_label(
            model.as_deref().or(agent.as_deref()),
            model.as_deref(),
        )
        .unwrap_or_else(|| runtime.actor_label().to_string()),
    )
    .filter(|value| !value.trim().is_empty());
    let result = match child_admission(reservation_owner)? {
        Some(admission) => runtime.submit_child_pipeline_run(
            &job_name,
            payload,
            priority.as_deref(),
            actor.as_deref(),
            &admission,
        )?,
        None => ChildSubmission::Submitted(runtime.submit_pipeline_run(
            &job_name,
            payload,
            priority.as_deref(),
            actor.as_deref(),
        )?),
    };
    match result {
        ChildSubmission::Submitted(result) => {
            serde_json::to_value(result).map_err(serialize_error("serialize pipeline invoke"))
        }
        ChildSubmission::Skipped(reason) => Ok(serde_json::json!({
            "skipped": true,
            "reason": reason,
            "job_name": job_name,
        })),
    }
}

fn child_admission(
    reservation_owner: Option<ReservationOwnerContext>,
) -> Result<Option<ChildPipelineAdmission>, OrbitError> {
    let Some(owner) = reservation_owner else {
        return Ok(None);
    };
    let Some(metadata) = owner.owner_metadata_json else {
        return Ok(None);
    };
    let metadata: Value = serde_json::from_str(&metadata).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid trusted reservation owner metadata: {error}"
        ))
    })?;
    let Some(admission) = metadata.get("pipeline_child_admission") else {
        return Ok(None);
    };
    let action = required_string(admission, &["action"], "action")?;
    let blocking = admission
        .get("blocking")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "trusted pipeline child admission is missing boolean `blocking`".to_string(),
            )
        })?;
    let parent_step_id = optional_string(admission, "parent_step_id")?;
    Ok(Some(ChildPipelineAdmission {
        parent_run_id: owner.owner_run_id,
        parent_step_id,
        action,
        blocking,
    }))
}

pub(super) fn wait(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let run_ids = parse_string_array_field(&input, "run_ids")?;
    let timeout_seconds =
        OrbitRuntime::normalize_pipeline_wait_timeout(parse_optional_timeout_seconds(&input)?)?;
    let poll_interval_seconds = OrbitRuntime::normalize_pipeline_wait_poll_interval(
        parse_optional_poll_interval_seconds(&input)?,
    );
    let actor = Some(
        normalize_optional_attribution_label(
            model.as_deref().or(agent.as_deref()),
            model.as_deref(),
        )
        .unwrap_or_else(|| runtime.actor_label().to_string()),
    )
    .filter(|value| !value.trim().is_empty());
    serde_json::to_value(runtime.wait_pipeline_runs(
        &run_ids,
        timeout_seconds,
        poll_interval_seconds,
        actor.as_deref(),
    )?)
    .map_err(serialize_error("serialize pipeline wait"))
}
