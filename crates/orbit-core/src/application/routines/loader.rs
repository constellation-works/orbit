//! Core assembly for routine sources and target catalog resolution.

use crate::OrbitRuntime;
use crate::application::routines::template::sync_retires_routine;
pub use orbit_automation::routines::loader::{
    LoadedRoutine, RetiredRoutine, RoutineCatalogLookup, RoutineCollection, RoutineLoadError,
    RoutineOrigin, RoutineSource,
};
use orbit_automation::routines::loader::{
    ROUTINES_DIR, manual_retirement_advice, retired_routine_job_reason, retired_routine_reason,
};
use orbit_common::OrbitError;
use orbit_types::workflow::RoutineDefinition;
use orbit_types::workspace::Workspace;
use std::collections::{BTreeMap, BTreeSet};
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
    /// Active replica checkouts. The sweep delivers the pull settlements
    /// their follower drains recorded and fires only their
    /// [replica-local](runs_in_replica) routines; no other routine, auto-task
    /// or task recovery runs in them.
    pub replicas: Vec<(Workspace, OrbitRuntime)>,
}

/// The one shipped job a replica checkout schedules for itself [ORB-14173].
///
/// Worktree GC reclaims this host's own run worktrees and reads task
/// settlement from the owner over its tool surface, so it needs no write to
/// the owner's coordination store. Every other routine target either mints or
/// moves owner tasks or ships work, and stays with the owner.
pub const REPLICA_LOCAL_ROUTINE_JOB: &str = "worktree_gc_pipeline";

/// Whether a replica checkout may schedule `definition` on this host's clock:
/// a cron routine targeting [`REPLICA_LOCAL_ROUTINE_JOB`]. Delivery and state
/// triggers observe owner coordination state, so they are owner-only even for
/// that job. Listing, toggling and the sweep all ask this one rule.
pub fn runs_in_replica(definition: &RoutineDefinition) -> bool {
    definition.target.job_name() == REPLICA_LOCAL_ROUTINE_JOB
        && definition.trigger.deliveries_landed.is_none()
        && definition.trigger.state.is_none()
}

/// Why a replica checkout does not schedule `definition`, naming the owner
/// whose checkout does.
pub fn replica_owner_only_reason(definition: &RoutineDefinition, owner_machine: &str) -> String {
    format!(
        "replica checkout schedules only host-local worktree GC (job:{REPLICA_LOCAL_ROUTINE_JOB} \
         on a cron trigger); '{}' -> {} is owner work: enable it in the owner checkout on \
         machine '{owner_machine}'",
        definition.name,
        definition.target.as_ref_string(),
    )
}

/// A routine a replica checkout defines that only its owner may schedule.
/// Reported so every surface can explain it; never fired.
#[derive(Debug, Clone)]
pub struct OwnerOnlyRoutine {
    /// The loaded definition plus its provenance.
    pub routine: LoadedRoutine,
    /// The machine that owns the workspace's coordination store.
    pub owner_machine: String,
    /// Operator-facing explanation naming the owner.
    pub reason: String,
}

/// Everything this host's discovered checkouts define: the routines it
/// schedules (owner routines and replica-local ones) and the replica
/// definitions it defers to their owners.
pub struct HostRoutines {
    /// Schedulable routines, retired definitions and load errors.
    pub collection: RoutineCollection,
    /// Replica definitions that only the owner may schedule.
    pub owner_only: Vec<OwnerOnlyRoutine>,
}

/// Collect routines from owner and replica checkouts together, so name
/// collisions across them still fail closed, then keep only the
/// [replica-local](runs_in_replica) routines a replica defines.
pub fn collect_host_routines(discovered: &DiscoveredWorkspaces) -> HostRoutines {
    let workspaces = discovered
        .entries
        .iter()
        .chain(&discovered.replicas)
        .collect::<Vec<_>>();
    let mut collection = collect_routines(&workspaces);
    let replica_owners: BTreeMap<PathBuf, String> = discovered
        .replicas
        .iter()
        .map(|(workspace, runtime)| {
            let owner = runtime
                .coordination_write_owner()
                .map(str::to_owned)
                .or_else(|| workspace.owner_machine_id.clone())
                .unwrap_or_else(|| "its owner".to_string());
            (runtime.shared_root(), owner)
        })
        .collect();
    let mut owner_only = Vec::new();
    collection.routines.retain(|routine| {
        let Some(owner) = replica_owners.get(&routine.source_orbit_dir) else {
            return true;
        };
        if runs_in_replica(&routine.definition) {
            return true;
        }
        owner_only.push(OwnerOnlyRoutine {
            routine: routine.clone(),
            owner_machine: owner.clone(),
            reason: replica_owner_only_reason(&routine.definition, owner),
        });
        false
    });
    HostRoutines {
        collection,
        owner_only,
    }
}

impl DiscoveredWorkspaces {
    /// One selected checkout as the whole discovery scope, placed by its own
    /// role: a replica checkout contributes only its replica-local routines.
    pub(crate) fn single_checkout(workspace: Workspace, runtime: OrbitRuntime) -> Self {
        let checkout = vec![(workspace, runtime)];
        if checkout[0].1.coordination_write_owner().is_some() {
            Self {
                replicas: checkout,
                ..Self::default()
            }
        } else {
            Self {
                entries: checkout,
                ..Self::default()
            }
        }
    }
}

/// Registry-neutral source of workspace runtimes for routine status and sweep.
/// Implementations may consult a catalog, but Core only observes the prepared
/// workspaces and fail-closed discovery errors.
pub trait RoutineWorkspaceProvider {
    fn discover_workspaces(&self, global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError>;
}

fn collect_routines(workspaces: &[&(Workspace, OrbitRuntime)]) -> RoutineCollection {
    let job_names_by_root = preload_job_execution_names(workspaces);
    let sources = workspaces
        .iter()
        .map(|(workspace, runtime)| RoutineSource {
            workspace: workspace.name.clone(),
            orbit_dir: runtime.shared_root(),
        })
        .collect::<Vec<_>>();
    let plugin_states = workspace_plugin_states(workspaces);
    let mut collection =
        collect_from_sources(&sources, &job_names_by_root, &move |path, _definition| {
            inactive_plugin_skip(path, &plugin_states)
        });
    narrow_retired_advice(&sources, &mut collection);
    collection
}

/// The routines directory as discovery spells the paths under it.
///
/// Discovery resolves symlinks in the Orbit directory before it lists, so every
/// loaded or retired routine path is symlink-free. A caller that matches those
/// paths against a workspace's routines directory has to resolve the directory
/// the same way, or a workspace reached through a symlinked ancestor (macOS
/// `/tmp` and `/var`, a symlinked home) never matches its own files.
fn discovered_routines_dir(orbit_dir: &Path) -> PathBuf {
    std::fs::canonicalize(orbit_dir)
        .unwrap_or_else(|_| orbit_dir.to_path_buf())
        .join(ROUTINES_DIR)
}

/// One discovered workspace's plugin surface, keyed by its routines
/// directory so a seeded file is judged by the workspace it lives in.
struct WorkspacePluginState {
    workspace: String,
    routines_dir: PathBuf,
    active: BTreeSet<String>,
    /// Host-enabled plugins this workspace's toggle switched off.
    switched_off: BTreeSet<String>,
}

/// Plugin installs are host-local, but each workspace narrows them with its
/// own `[plugin_enablement]` toggles, so the active set is per workspace.
fn workspace_plugin_states(workspaces: &[&(Workspace, OrbitRuntime)]) -> Vec<WorkspacePluginState> {
    workspaces
        .iter()
        .map(|(workspace, runtime)| {
            let load = runtime.plugin_load();
            WorkspacePluginState {
                workspace: workspace.name.clone(),
                routines_dir: discovered_routines_dir(&runtime.shared_root()),
                active: load
                    .active()
                    .map(|plugin| plugin.namespace().to_string())
                    .collect(),
                switched_off: load
                    .registered
                    .iter()
                    .filter(|entry| load.is_disabled_in_workspace(&entry.name))
                    .map(|entry| entry.name.clone())
                    .collect(),
            }
        })
        .collect()
}

impl crate::application::plugin::PluginActivity for WorkspacePluginState {
    fn is_active(&self, namespace: &str) -> bool {
        self.active.contains(namespace)
    }

    fn is_disabled_in_workspace(&self, namespace: &str) -> bool {
        self.switched_off.contains(namespace)
    }
}

/// The host-wide view, for a path outside every discovered source: a plugin
/// counts as active when any workspace has it on, and no workspace toggle
/// applies.
struct HostPluginActivity<'a>(&'a [WorkspacePluginState]);

impl crate::application::plugin::PluginActivity for HostPluginActivity<'_> {
    fn is_active(&self, namespace: &str) -> bool {
        self.0.iter().any(|state| state.active.contains(namespace))
    }

    fn is_disabled_in_workspace(&self, _namespace: &str) -> bool {
        false
    }
}

/// Skip a definition a plugin seeded while that plugin is disabled, removed,
/// or switched off in the workspace the definition lives in.
///
/// The seeded file stays on disk with the operator's edits; it simply does not
/// fire, and the reason names the plugin (design §4.5). This is deliberately
/// not a load error: a disabled plugin is an ordinary operator state, not a
/// broken workspace. The judgement is the shared
/// [`inactive_plugin`](crate::application::plugin::inactive_plugin) rule, so
/// `routine list` hides exactly what the sweep skips.
fn inactive_plugin_skip(path: &Path, states: &[WorkspacePluginState]) -> Option<String> {
    use crate::application::plugin::inactive_plugin;
    if let Some(state) = states
        .iter()
        .find(|state| path.starts_with(&state.routines_dir))
    {
        return inactive_plugin(&state.routines_dir, path, state)
            .map(|inactive| inactive.reason(path, Some(&state.workspace)));
    }
    // A collected path can miss the lexical prefix when an ancestor is a
    // symlink (macOS `/tmp`). Resolve it and read only when that canonical
    // file sits inside a discovered routines directory and is not itself a
    // symlink. A path outside every discovered directory is not opened.
    let canonical = std::fs::canonicalize(path).ok()?;
    if std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return None;
    }
    let state = states
        .iter()
        .find(|state| canonical.starts_with(&state.routines_dir))?;
    inactive_plugin(&state.routines_dir, &canonical, &HostPluginActivity(states))
        .map(|inactive| inactive.reason(path, None))
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
                discovered_routines_dir(&source.orbit_dir),
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
    skip: &orbit_automation::routines::loader::RoutineSkipRule<'_>,
) -> RoutineCollection {
    orbit_automation::routines::loader::collect_routines_with_skips(
        sources,
        &|root, job| {
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
        },
        skip,
    )
}

fn preload_job_execution_names(
    workspaces: &[&(Workspace, OrbitRuntime)],
) -> BTreeMap<PathBuf, crate::application::job::catalog::V2JobExecutionMembership> {
    let mut job_names_by_root = BTreeMap::new();
    for (_, runtime) in workspaces {
        job_names_by_root
            .entry(runtime.shared_root())
            .or_insert_with(|| runtime.load_v2_job_execution_membership());
    }
    job_names_by_root
}
