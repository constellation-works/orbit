use std::collections::BTreeSet;

use orbit_types::identity::agent_from_model;
use orbit_types::task::Task;

/// Render task-scoped orchestration attribution. `created_by` carries the
/// actor's exact model at task creation; it is relevant here only when the
/// task also records an explicit orchestrator. That prevents creation or
/// implementation attribution from being mistaken for orchestration.
pub(super) fn orchestration_trailer(tasks: &[Task]) -> Option<String> {
    let values = tasks
        .iter()
        .filter_map(orchestration_attribution)
        .collect::<BTreeSet<_>>();

    (!values.is_empty()).then(|| {
        format!(
            "Orchestrated-By: {}",
            values.into_iter().collect::<Vec<_>>().join(", ")
        )
    })
}

fn orchestration_attribution(task: &Task) -> Option<String> {
    let orchestrator = trailer_value(task.orchestrator.as_deref())?;
    let model = task
        .created_by
        .as_deref()
        .and_then(|value| trailer_value(Some(value)))
        .filter(|value| agent_from_model(value).is_some());

    model.or(Some(orchestrator))
}

fn trailer_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && !value.contains(['\r', '\n'])).then(|| value.to_string())
}
