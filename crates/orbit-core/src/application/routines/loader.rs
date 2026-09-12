//! Core assembly for routine sources and target catalog resolution.

use crate::OrbitRuntime;
pub use orbit_automation::routines::loader::{
    LoadedRoutine, RoutineCollection, RoutineLoadError, RoutineOrigin, RoutineSource,
};
use orbit_common::OrbitError;
use orbit_types::workspace::Workspace;
use std::path::Path;

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
    let sources = workspaces
        .iter()
        .map(|(workspace, runtime)| RoutineSource {
            workspace: workspace.name.clone(),
            orbit_dir: runtime.shared_root(),
        })
        .collect::<Vec<_>>();
    orbit_automation::routines::loader::collect_routines(&sources, &|root, job| {
        workspaces
            .iter()
            .find(|(_, runtime)| runtime.shared_root() == root)
            .is_some_and(|(_, runtime)| runtime.load_v2_job_asset_by_name(job).is_ok())
    })
}
