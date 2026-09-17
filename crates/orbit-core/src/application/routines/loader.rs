//! Core assembly for routine sources and target catalog resolution.

use crate::OrbitRuntime;
use crate::application::routine::sync_retires_routine;
pub use orbit_automation::routines::loader::{
    LoadedRoutine, RetiredRoutine, RoutineCatalogLookup, RoutineCollection, RoutineLoadError,
    RoutineOrigin, RoutineSource,
};
use orbit_automation::routines::loader::{
    ROUTINES_DIR, manual_retirement_advice, retired_routine_job_reason, retired_routine_reason,
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
    let mut collection = collect_from_sources(&sources, &job_names_by_root);
    narrow_retired_advice(&sources, &mut collection);
    collection
}

/// Discovery states the synchronization step for every retired definition
/// because it cannot tell one Orbit seeded from one the operator wrote. Core
/// owns the managed-routine templates and the manifest, so it is the layer
/// that can: replace the advice wherever synchronization would leave the file
/// exactly where it is, so the operator is never sent back to a command that
/// reports `unchanged` forever [DANI-10502].
fn narrow_retired_advice(sources: &[RoutineSource], collection: &mut RoutineCollection) {
    let routines_dirs: BTreeMap<&str, PathBuf> = sources
        .iter()
        .map(|source| {
            (
                source.workspace.as_str(),
                source.orbit_dir.join(ROUTINES_DIR),
            )
        })
        .collect();
    for retired in &mut collection.retired {
        let Some(routines_dir) = routines_dirs.get(retired.source_workspace.as_str()) else {
            continue;
        };
        if sync_retires_routine(routines_dir, &retired.path) {
            continue;
        }
        let Some(retirement) = retired_routine_job_reason(&retired.job) else {
            continue;
        };
        retired.reason = retired_routine_reason(
            &retired.job,
            retirement,
            &manual_retirement_advice(&retired.path),
        );
    }
}

fn collect_from_sources(
    sources: &[RoutineSource],
    job_names_by_root: &BTreeMap<
        PathBuf,
        crate::application::job::catalog::V2JobExecutionMembership,
    >,
) -> RoutineCollection {
    orbit_automation::routines::loader::collect_routines(sources, &|root, job| {
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
