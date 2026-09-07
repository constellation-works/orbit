use std::collections::BTreeSet;

use orbit_types::task::Task;

/// Render task-scoped orchestration attribution from the explicit ownership
/// record. Creation and implementation provenance are separate identities, so
/// neither can establish a model for the task's orchestrator.
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
    trailer_value(task.orchestrator.as_deref())
}

fn trailer_value(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && !value.contains(['\r', '\n'])).then(|| value.to_string())
}
