//! Dispatch lineage for the dashboard's run lists.
//!
//! History folds a task's pipeline runs into one group per attempt and names
//! the failure of the run that failed on its own, not the success guard of a
//! parent that echoed it. Both need pipeline states for listed runs and their
//! retry sources, read together in one store round-trip.

use std::collections::{HashMap, HashSet};

use orbit_core::{JobRun, OrbitRuntime};
use serde_json::{Value, json};

/// Set `parent_run_id` and `root_cause` on each listed run's value.
///
/// `parent_run_id` is the listed run whose own child dispatches name this run,
/// null when no run in the same listing dispatched it. `root_cause` is the
/// leaf a cascaded failure echoes, null for a run that failed on its own.
/// When the states cannot be read the listing keeps its plain shape without
/// `parent_run_id`, so History falls back to one group per task.
pub(in crate::api) fn add_run_lineage(
    runtime: &OrbitRuntime,
    runs: &[JobRun],
    values: &mut [Value],
) {
    let mut run_ids = runs
        .iter()
        .map(|run| run.run_id.clone())
        .chain(
            runs.iter()
                .filter_map(|run| run.retry_source_run_id.clone()),
        )
        .collect::<Vec<_>>();
    run_ids.sort_unstable();
    run_ids.dedup();
    let states = match runtime.read_run_states(&run_ids) {
        Ok(states) => states,
        Err(error) => {
            tracing::warn!(%error, "run list omitted dispatch lineage");
            return;
        }
    };
    let mut parents: HashMap<&str, &str> = HashMap::new();
    for run in runs {
        let Some(state) = states.get(&run.run_id).and_then(Option::as_ref) else {
            continue;
        };
        // Resume preserves dispatch checkpoints for recovery. A copied child
        // still belongs to the source attempt, even when that source is not
        // listed on this page. Only new dispatches belong to the retry.
        let inherited_children = run
            .retry_source_run_id
            .as_ref()
            .and_then(|source| states.get(source))
            .and_then(Option::as_ref)
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
        let state = states.get(&run.run_id).and_then(Option::as_ref);
        value["parent_run_id"] = json!(parents.get(run.run_id.as_str()));
        value["root_cause"] = json!(state.and_then(|state| state.root_cause.as_ref()));
    }
}
