//! Rows of the dashboard's run lists.

use std::collections::BTreeMap;

use orbit_core::application::job::job_run_to_json;
use orbit_core::{JobRun, OrbitRuntime};
use serde_json::Value;

use super::lineage::add_run_lineage;
use super::tasks::add_tasks;

/// Project a run-list page: each run with its pipeline state, its tasks and
/// its dispatch lineage.
///
/// History names a cascaded failure's leaf from the shared row's
/// `root_cause` and groups a task's runs per attempt from `parent_run_id`.
/// When the states cannot be read, the rows keep their stateless shape
/// without `parent_run_id`, so History falls back to one group per task.
pub(in crate::api) fn run_list_rows(
    runtime: &OrbitRuntime,
    runs: &[JobRun],
    titles: &BTreeMap<String, String>,
) -> Vec<Value> {
    let states = runtime
        .read_job_run_list_states(runs)
        .inspect_err(|error| {
            tracing::warn!(%error, "run list omitted pipeline state and dispatch lineage");
        })
        .ok();
    let mut values = runs
        .iter()
        .map(|run| {
            let mut value = match &states {
                Some(states) => states.row(run),
                None => job_run_to_json(run, None),
            };
            add_tasks(&mut value, run, titles);
            value
        })
        .collect::<Vec<_>>();
    if let Some(states) = &states {
        add_run_lineage(runs, states, &mut values);
    }
    values
}
