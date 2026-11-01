//! Read-only probe for workflow runs that still hold a workspace.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_store::compose::workspace_job_run_store;
use orbit_store::contracts::JobRunQuery;
use orbit_store::maintenance::task_registry::read_workspace_config_optional;
use orbit_types::workflow::JobRunState;

use crate::application::job::running_run_owner_is_stale;

/// Run IDs of the workflow runs of one workspace that have not finished:
/// `pending`, `running` and `retrying` (a retrying run sleeps between
/// attempts and then executes again). A drain is one such run, so a live
/// drain is included. A `running` run whose owner is conclusively gone
/// (`Missing` or `Mismatch` after the startup grace, per
/// `running_run_owner_is_stale`) no longer holds the workspace and is
/// excluded unless its persisted pipeline state marks it as parked; a parked
/// run deliberately has no live worker but still holds the workspace until it
/// resumes. Inconclusive or live owners also still hold it.
///
/// This opens the host store `<global_root>/orbit.db` read-only, without
/// creating it, migrating it or opening a runtime, so a caller that must not
/// disturb the workspace (convergence) can ask before it writes. A host with
/// no store, or a workspace never bound to one, has no runs.
pub fn workspace_live_run_ids(
    global_root: &Path,
    workspace_orbit_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    let database = global_root.join("orbit.db");
    if !database.is_file() {
        return Ok(Vec::new());
    }
    let Some(config) = read_workspace_config_optional(workspace_orbit_root)? else {
        return Ok(Vec::new());
    };
    let workspace_id = config.workspace_id;
    let store = Store::open_read_only(&database)?;
    let runs = workspace_job_run_store(store.clone(), workspace_id.clone())
        .list_job_runs_filtered(&JobRunQuery {
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            include_steps: false,
            ..JobRunQuery::default()
        })?;
    let stale_owner_run_ids: BTreeSet<String> = runs
        .iter()
        .filter(|run| running_run_owner_is_stale(run))
        .map(|run| run.run_id.clone())
        .collect();
    let stale_owner_states = store.read_job_run_states_for_workspace(
        &workspace_id,
        &stale_owner_run_ids.iter().cloned().collect::<Vec<_>>(),
    )?;
    let mut run_ids: Vec<String> = runs
        .into_iter()
        .filter(|run| {
            !stale_owner_run_ids.contains(&run.run_id)
                || stale_owner_states
                    .get(&run.run_id)
                    .and_then(Option::as_ref)
                    .is_some_and(|state| state.parked_wait().is_some())
        })
        .map(|run| run.run_id)
        .collect();
    run_ids.sort();
    Ok(run_ids)
}
