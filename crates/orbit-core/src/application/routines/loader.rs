//! Core assembly for routine sources and target catalog resolution.

use crate::OrbitRuntime;
pub use orbit_automation::routines::loader::{
    LoadedRoutine, RetiredRoutine, RoutineCatalogLookup, RoutineCollection, RoutineLoadError,
    RoutineOrigin, RoutineSource,
};
use orbit_common::OrbitError;
use orbit_types::workspace::Workspace;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Registered workspaces with their runtimes, ready for routine discovery
/// and dispatch, plus the workspaces that failed to open (reported loudly —
/// registry hygiene must not silently shrink the source set).
#[derive(Default)]
pub struct DiscoveredWorkspaces {
    /// Active, openable owner checkouts with their runtimes.
    pub entries: Vec<(Workspace, OrbitRuntime)>,
    /// Registered workspaces that could not be opened.
    pub errors: Vec<RoutineLoadError>,
}

/// Registry-neutral source of workspace runtimes for routine status and sweep.
/// Implementations may consult a catalog, but Core only observes the prepared
/// workspaces and fail-closed discovery errors.
pub trait RoutineWorkspaceProvider {
    fn discover_workspaces(&self, global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError>;
}

pub fn collect_routines(workspaces: &[(Workspace, OrbitRuntime)]) -> RoutineCollection {
    let job_names_by_root = preload_job_execution_names(workspaces);
    let sources = workspaces
        .iter()
        .map(|(workspace, runtime)| RoutineSource {
            workspace: workspace.name.clone(),
            orbit_dir: runtime.shared_root(),
        })
        .collect::<Vec<_>>();
    orbit_automation::routines::loader::collect_routines(&sources, &|root, job| {
        job_names_by_root
            .get(root)
            .map(|membership| RoutineCatalogLookup {
                resolves: membership.names.contains(job),
                error: (!membership.errors.is_empty()).then(|| {
                    membership
                        .errors
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                }),
            })
            .unwrap_or_default()
    })
}

fn preload_job_execution_names(
    workspaces: &[(Workspace, OrbitRuntime)],
) -> BTreeMap<PathBuf, crate::application::job::catalog::V2JobExecutionMembership> {
    let mut job_names_by_root = BTreeMap::new();
    for (_, runtime) in workspaces {
        job_names_by_root
            .entry(runtime.shared_root())
            .or_insert_with(|| runtime.load_v2_job_execution_membership());
    }
    job_names_by_root
}
