use orbit_common::OrbitError;
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::task::Task;
use serde_json::json;

use crate::context::RuntimeHost;

pub(in crate::executor::automation) fn ship_done_attribution(task: &Task) -> Option<String> {
    normalize_optional_attribution_label(
        task.implemented_by
            .as_deref()
            .or(task.created_by.as_deref()),
        task.implemented_by.as_deref(),
    )
}

pub(super) fn pr_review_attribution<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
    batch_id: &str,
) -> Result<Option<String>, OrbitError> {
    if let Some(existing) =
        normalize_optional_attribution_label(task.implemented_by.as_deref(), None)
    {
        return Ok(Some(existing));
    }

    let run_id = task.job_run_id.as_deref().unwrap_or(batch_id);
    let identity_input = json!({
        "task_id": task.id,
        "run_id": run_id,
    });
    let (agent, model) = host.activity_implementer_identity(&identity_input)?;
    Ok(normalize_optional_attribution_label(
        model
            .as_deref()
            .or(agent.as_deref())
            .or(task.planned_by.as_deref())
            .or(task.created_by.as_deref()),
        model.as_deref(),
    ))
}
