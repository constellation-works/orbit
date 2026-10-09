//! Dashboard task labels derived from persisted run input.

use std::collections::{BTreeMap, BTreeSet};

use orbit_core::{JobRun, OrbitError, OrbitRuntime};
use orbit_types::task::is_valid_orb_task_id;
use serde_json::{Value, json};

use orbit_core::application::job::job_run_task_ids as task_ids;

/// Titles of the tasks the selected runs reference: a keyed read per
/// referenced id, never a workspace listing. Run input is free-form, so ids
/// that are not task ids are never read and stay unlabelled.
pub(super) fn task_titles<'a>(
    runtime: &OrbitRuntime,
    runs: impl IntoIterator<Item = &'a JobRun>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let ids: BTreeSet<_> = runs
        .into_iter()
        .flat_map(task_ids)
        .filter(|id| is_valid_orb_task_id(id))
        .collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    Ok(runtime
        .list_task_metadata_for_ids(&ids)?
        .into_iter()
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
