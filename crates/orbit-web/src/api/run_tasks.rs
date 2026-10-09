//! Dashboard task labels derived from persisted run input.

use std::collections::{BTreeMap, BTreeSet};

use orbit_core::{JobRun, OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use orbit_core::application::job::job_run_task_ids as task_ids;

/// One metadata listing for the selected runs, never a task-body read per row.
pub(super) fn task_titles<'a>(
    runtime: &OrbitRuntime,
    runs: impl IntoIterator<Item = &'a JobRun>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let ids: BTreeSet<_> = runs.into_iter().flat_map(task_ids).collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(runtime
        .list_task_metadata()?
        .into_iter()
        .filter(|task| ids.contains(&task.id))
        .map(|task| (task.id, task.title))
        .collect())
}

pub(super) fn add_tasks(value: &mut Value, run: &JobRun, titles: &BTreeMap<String, String>) {
    let ids = task_ids(run);
    if ids.is_empty() {
        value["task_ids"] = Value::Null;
        value["tasks"] = Value::Null;
    } else {
        value["tasks"] = json!(
            ids.iter()
                .map(|id| json!({
                    "id": id,
                    "title": titles.get(id),
                }))
                .collect::<Vec<_>>()
        );
        value["task_ids"] = json!(ids);
    }
}
