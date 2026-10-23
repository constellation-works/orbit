//! Dispatch lineage for the dashboard's run lists.
//!
//! History folds a task's pipeline runs into one group per attempt from the
//! listed runs' child dispatches and their retry sources'.

use std::collections::{HashMap, HashSet};

use orbit_core::JobRun;
use orbit_core::application::job::JobRunListStates;
use serde_json::{Value, json};

/// Set `parent_run_id` on each listed run's value.
///
/// `parent_run_id` is the listed run whose own child dispatches name this run,
/// null when no run in the same listing dispatched it. The states are the
/// page's one read, which also covers each listed run's retry source.
pub(super) fn add_run_lineage(runs: &[JobRun], states: &JobRunListStates, values: &mut [Value]) {
    let mut parents: HashMap<&str, &str> = HashMap::new();
    for run in runs {
        let Some(state) = states.state(&run.run_id) else {
            continue;
        };
        // Resume preserves dispatch checkpoints for recovery. A copied child
        // still belongs to the source attempt, even when that source is not
        // listed on this page. Only new dispatches belong to the retry.
        let inherited_children = run
            .retry_source_run_id
            .as_ref()
            .and_then(|source| states.state(source))
            .into_iter()
            .flat_map(|source| &source.child_dispatches)
            .map(|dispatch| dispatch.child_run_id.as_str())
            .collect::<HashSet<_>>();
        for dispatch in &state.child_dispatches {
            if inherited_children.contains(dispatch.child_run_id.as_str()) {
                continue;
            }
            parents
                .entry(dispatch.child_run_id.as_str())
                .or_insert(run.run_id.as_str());
        }
    }
    for (run, value) in runs.iter().zip(values.iter_mut()) {
        value["parent_run_id"] = json!(parents.get(run.run_id.as_str()));
    }
}
