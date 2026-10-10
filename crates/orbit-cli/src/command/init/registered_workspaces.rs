//! Create the workspace-local defaults a release ships, in every workspace
//! registered on this host, as part of `orbit init`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_core::{
    ManagedArtifactOutcome, RoutineSeedIdentity, seed_absent_workspace_managed_artifacts,
};
use orbit_registry::workspace_registry;

/// The defaults `orbit init` created in one registered workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkspaceSeed {
    pub(super) workspace: String,
    pub(super) orbit_root: PathBuf,
    pub(super) created: Vec<PathBuf>,
}

#[derive(Debug, Default)]
pub(super) struct RegisteredWorkspaceSeeds {
    pub(super) seeds: Vec<WorkspaceSeed>,
    pub(super) warnings: Vec<String>,
}

/// Create the absent workspace defaults in every checkout registered under
/// `global_root`.
///
/// A failure is a warning, not an init failure: the host-global defaults have
/// already landed, and `orbit workspace sync` inside the affected workspace
/// converges it and reports the same problem in full.
pub(super) fn seed_registered_workspaces(
    global_root: &Path,
    machine_id: &str,
) -> RegisteredWorkspaceSeeds {
    let mut result = RegisteredWorkspaceSeeds::default();
    let registry_path = workspace_registry::registry_path_for(global_root);
    let registry = match workspace_registry::load_registry_from(&registry_path) {
        Ok(registry) => registry,
        Err(error) => {
            result.warnings.push(format!(
                "registered workspaces were not seeded: {error}; run `orbit workspace sync` in each workspace"
            ));
            return result;
        }
    };
    // A checkout whose `.orbit/` is gone is not seeded: creating the catalog
    // would recreate a directory the operator removed.
    let mut seeded_roots = BTreeSet::new();
    for checkout in &registry.checkouts {
        if !checkout.orbit_dir.is_dir() || !seeded_roots.insert(checkout.orbit_dir.clone()) {
            continue;
        }
        let Some(workspace) =
            workspace_registry::find_workspace_by_id(&registry, &checkout.workspace_id)
        else {
            continue;
        };
        let seeded = RoutineSeedIdentity::new(&workspace.name, machine_id, &workspace.base_branch)
            .and_then(|identity| {
                seed_absent_workspace_managed_artifacts(
                    global_root,
                    &checkout.orbit_dir,
                    &identity,
                    &workspace.base_branch,
                )
            });
        match seeded {
            Ok(report) => {
                result.warnings.extend(report.warnings);
                result.seeds.push(WorkspaceSeed {
                    workspace: workspace.name.clone(),
                    orbit_root: checkout.orbit_dir.clone(),
                    created: report
                        .actions
                        .into_iter()
                        .filter(|action| action.outcome == ManagedArtifactOutcome::Created)
                        .map(|action| action.path)
                        .collect(),
                });
            }
            Err(error) => result.warnings.push(format!(
                "workspace `{}` at '{}' was not seeded: {error}; run `orbit workspace sync` in it",
                workspace.name,
                checkout.orbit_dir.display()
            )),
        }
    }
    result
}
