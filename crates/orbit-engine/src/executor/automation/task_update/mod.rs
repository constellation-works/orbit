mod complete;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskActivityUpdate};

use super::StateExecutionContext;
use super::input::{input_string_field, required_input_string};

pub(in crate::executor::automation) use complete::{
    authorization_note, complete_tasks, task_complete,
};

pub(super) fn update_task<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    state_context: Option<&StateExecutionContext>,
) -> Result<Value, OrbitError> {
    let task_id = required_input_string(input, "task_id")?;

    // Tolerate missing status: when the upstream agent persisted status directly
    // to the task (via orbit.task.update) but crashed before returning structured
    // output, piped input will lack `status`. In that case, treat as a no-op
    // since the task already has the correct status.
    let status = match input_string_field(input, "status") {
        Some(raw) => raw
            .parse::<TaskStatus>()
            .map_err(|error| OrbitError::InvalidInput(format!("invalid input.status: {error}")))?,
        None => return Ok(json!({})),
    };

    // Idempotent: if task is already at the target status, skip the update.
    let task = host.get_task(task_id)?;
    if task.status == status {
        return Ok(json!({}));
    }

    let note = input_string_field(input, "note")
        .or_else(|| Some(format!("automation: update_task → {status}")));
    let (agent, model) = activity_identity(host, input, state_context)?;
    host.update_task_from_activity(
        task_id,
        TaskActivityUpdate {
            status,
            // ORB-10603: this step never authors the execution summary. The
            // durable summary is written upstream — by the implementing agent,
            // or derived from the delivered change by the commit step — and a
            // status transition must not clobber it.
            execution_summary: None,
            comment: None,
            note,
            agent,
            model,
        },
    )?;
    Ok(json!({}))
}

fn activity_identity<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    state_context: Option<&StateExecutionContext>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    if let Some(context) = state_context {
        let agent = non_empty(context.agent.as_deref());
        let model = non_empty(context.model.as_deref());
        if agent.is_some() || model.is_some() {
            return Ok((agent, model));
        }
    }

    host.activity_implementer_identity(input)
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
