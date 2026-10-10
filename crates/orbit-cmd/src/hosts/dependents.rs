//! What on this machine routes to a host: the local replica checkouts it owns
//! and the pull drains pending or running in them.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_registry::hosts::{ResolvedHost, load_host_registry};
use orbit_registry::workspace_registry::{
    find_workspace_by_id, load_registry_from_read_only, registry_path_for,
};
use orbit_types::workflow::JobRunState;
use orbit_types::workspace::{Workspace, WorkspaceCheckoutRole, WorkspaceRegistry};

use super::{HostDependents, PullDrainDependent, ReplicaDependent};

impl HostDependents {
    pub fn is_empty(&self) -> bool {
        self.replica_checkouts.is_empty() && self.pull_drains.is_empty()
    }

    /// One line per dependent, for refusals and human output.
    pub fn describe(&self) -> Vec<String> {
        self.replica_checkouts
            .iter()
            .map(|checkout| {
                format!(
                    "replica checkout {} ({}) at {}",
                    checkout.workspace_name,
                    checkout.workspace_id,
                    checkout.repo_root.display()
                )
            })
            .chain(self.pull_drains.iter().map(|drain| {
                format!(
                    "{} pull drain {} in {}",
                    drain.state, drain.run_id, drain.workspace_id
                )
            }))
            .collect()
    }
}

/// What on this machine routes to the host `selector` names: the dependents a
/// `host_in_use` refusal lists. `None` for the local host.
pub fn dependents_of(
    global_root: &Path,
    selector: &str,
) -> Result<Option<HostDependents>, OrbitError> {
    let registry = load_host_registry(global_root)?;
    match registry.resolve(selector)? {
        ResolvedHost::Local(_) => Ok(None),
        resolved => host_dependents(global_root, resolved.machine_id()).map(Some),
    }
}

/// Local replica checkouts whose owner is `machine_id`, and the pull drains
/// pending or running in them. A pull drain always runs in a replica checkout
/// of the owner its selector names, so the checkouts are the complete set to
/// inspect.
pub(super) fn host_dependents(
    global_root: &Path,
    machine_id: &str,
) -> Result<HostDependents, OrbitError> {
    let registry = load_registry_from_read_only(&registry_path_for(global_root))?.registry;
    let mut dependents = HostDependents::default();
    for (workspace, checkout) in replica_checkouts_of(&registry, machine_id) {
        dependents.replica_checkouts.push(ReplicaDependent {
            workspace_id: workspace.id.clone(),
            workspace_name: workspace.name.clone(),
            repo_root: checkout.repo_root.clone(),
        });
        let runtime = match crate::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout_read_only(
            global_root,
            workspace,
            checkout,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, workspace = %workspace.id, "cannot inspect pull drains");
                continue;
            }
        };
        for state in [JobRunState::Running, JobRunState::Pending] {
            let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
                job_id: Some(orbit_core::application::distributed::PULL_DRAIN_JOB.to_string()),
                state: Some(state),
                ..Default::default()
            });
            match runs {
                Ok(runs) => {
                    dependents
                        .pull_drains
                        .extend(runs.into_iter().map(|run| PullDrainDependent {
                            workspace_id: workspace.id.clone(),
                            run_id: run.run_id,
                            state: run.state.to_string(),
                        }))
                }
                Err(error) => {
                    tracing::warn!(%error, workspace = %workspace.id, "cannot list pull drains");
                }
            }
        }
    }
    Ok(dependents)
}

fn replica_checkouts_of<'a>(
    registry: &'a WorkspaceRegistry,
    machine_id: &'a str,
) -> impl Iterator<Item = (&'a Workspace, &'a orbit_types::workspace::WorkspaceCheckout)> + 'a {
    registry.checkouts.iter().filter_map(move |checkout| {
        let replica_of_host = checkout.role == Some(WorkspaceCheckoutRole::Replica)
            && checkout.owner_machine_id.as_deref() == Some(machine_id);
        if !replica_of_host {
            return None;
        }
        find_workspace_by_id(registry, &checkout.workspace_id)
            .map(|workspace| (workspace, checkout))
    })
}

/// Owners of this machine's replica checkouts: the hosts it pulls from.
pub(super) fn replica_owners(global_root: &Path) -> BTreeSet<String> {
    let Ok(load) = load_registry_from_read_only(&registry_path_for(global_root)) else {
        return BTreeSet::new();
    };
    load.registry
        .checkouts
        .iter()
        .filter(|checkout| checkout.role == Some(WorkspaceCheckoutRole::Replica))
        .filter_map(|checkout| checkout.owner_machine_id.clone())
        .collect()
}
