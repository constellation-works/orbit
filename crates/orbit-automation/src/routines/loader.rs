//! Routine discovery [ORB-10021]: enumerate the global workspace registry,
//! visit every registered, active workspace whose versioned config declares
//! `[routines] role = "source"` (ADR-0205), and load `.orbit/routines/*.yaml`
//! from each — fail-closed per file. An invalid definition becomes a load
//! error and that routine is treated as absent; it never fires with defaults.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::protocol::yaml::{parse_local_routine_yaml, parse_routine_yaml};
use orbit_types::workflow::RoutineDefinition;

use super::due::parse_cron;

/// Directory under a source workspace's `.orbit/` holding routine YAML files.
pub const ROUTINES_DIR: &str = "routines";

/// Subdirectory of [`ROUTINES_DIR`] holding machine-local routine definitions
/// (gitignored by convention). The directory is the origin contract — the
/// sweep never shells out to `git check-ignore` (host-registry design §6).
pub const LOCAL_ROUTINES_SUBDIR: &str = "local";

/// Where a routine definition came from — the directory decides, not git
/// status (host-registry design §6). Committed definitions must pin a host;
/// local definitions are implicitly pinned to the loading host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineOrigin {
    /// A git-committed definition under `.orbit/routines/` (excluding
    /// `local/`). Requires a non-empty explicit `hosts:` pin.
    Committed,
    /// A machine-local definition under `.orbit/routines/local/`. Implicitly
    /// pinned to the loading host; may not name another host.
    Local,
}

impl RoutineOrigin {
    /// Stable lowercase label for reporting (`committed` / `local`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Local => "local",
        }
    }
}

/// One successfully loaded, fully validated routine.
#[derive(Debug, Clone)]
pub struct LoadedRoutine {
    /// The parsed definition.
    pub definition: RoutineDefinition,
    /// Whether the definition is committed or machine-local.
    pub origin: RoutineOrigin,
    /// Registry name of the source workspace.
    pub source_workspace: String,
    /// The source workspace's `.orbit` directory (dispatch root).
    pub source_orbit_dir: PathBuf,
    /// Path of the YAML file the definition came from.
    pub path: PathBuf,
}

/// One fail-closed load failure, kept for reporting: the routine (or source)
/// it names is treated as absent this pass.
#[derive(Debug, Clone)]
pub struct RoutineLoadError {
    /// Registry name of the source workspace involved.
    pub source_workspace: String,
    /// File that failed, when the failure is file-scoped.
    pub path: Option<PathBuf>,
    /// Human-readable reason.
    pub message: String,
}

/// Result of one discovery pass across all routine sources.
#[derive(Debug, Default)]
pub struct RoutineCollection {
    /// Valid routines, in stable (workspace, filename) order.
    pub routines: Vec<LoadedRoutine>,
    /// Everything that failed fail-closed.
    pub errors: Vec<RoutineLoadError>,
}

/// Explicit source description prepared by Core/registry composition.
#[derive(Debug, Clone)]
pub struct RoutineSource {
    pub workspace: String,
    pub orbit_dir: PathBuf,
    pub enabled: bool,
}

/// Load routines from every source workspace among `workspaces` (the same
/// runtimes are later used for dispatch), origin-aware: committed definitions
/// under `.orbit/routines/` require a host pin, local definitions under
/// `.orbit/routines/local/` are implicit to `host_id`. Cross-origin name
/// collisions are load-time errors: every colliding definition is dropped and
/// each conflicting source is named.
pub fn collect_routines(
    workspaces: &[RoutineSource],
    catalog: &dyn Fn(&Path, &str) -> bool,
    host_id: &str,
) -> RoutineCollection {
    let mut collection = RoutineCollection::default();

    for source in workspaces {
        if !source.enabled {
            continue;
        }
        load_source_workspace(source, catalog, host_id, &mut collection);
    }

    drop_name_collisions(&mut collection);
    collection
}

fn load_source_workspace(
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> bool,
    host_id: &str,
    collection: &mut RoutineCollection,
) {
    let routines_dir = source.orbit_dir.join(ROUTINES_DIR);
    if !routines_dir.is_dir() {
        // A source with no routines directory is simply an empty source.
        return;
    }

    // Committed definitions: top-level YAML files. The `local/` subdirectory is
    // a directory (never a file) so it is skipped here and scanned separately.
    load_origin_dir(
        &routines_dir,
        RoutineOrigin::Committed,
        source,
        catalog,
        host_id,
        collection,
    );

    // Local definitions: `.orbit/routines/local/`, implicit to this host.
    let local_dir = routines_dir.join(LOCAL_ROUTINES_SUBDIR);
    if local_dir.is_dir() {
        load_origin_dir(
            &local_dir,
            RoutineOrigin::Local,
            source,
            catalog,
            host_id,
            collection,
        );
    }
}

/// Load every top-level YAML file in `dir` under `origin`. Only regular files
/// are considered, so a committed scan of `.orbit/routines/` never treats the
/// `local/` subdirectory as a definition.
fn load_origin_dir(
    dir: &Path,
    origin: RoutineOrigin,
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> bool,
    host_id: &str,
    collection: &mut RoutineCollection,
) {
    let paths = match yaml_files_in(dir) {
        Ok(paths) => paths,
        Err(error) => {
            collection.errors.push(RoutineLoadError {
                source_workspace: source.workspace.clone(),
                path: Some(dir.to_path_buf()),
                message: format!("failed to list routines directory: {error}"),
            });
            return;
        }
    };

    for path in paths {
        match load_routine_file(&path, origin, source, catalog, host_id) {
            Ok(routine) => collection.routines.push(routine),
            Err(message) => collection.errors.push(RoutineLoadError {
                source_workspace: source.workspace.clone(),
                path: Some(path),
                message,
            }),
        }
    }
}

/// Regular `*.yaml` / `*.yml` files directly in `dir`, in stable filename
/// order. Subdirectories (e.g. `local/` under the committed scan) are skipped.
fn yaml_files_in(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                })
        })
        .collect();
    paths.sort();
    Ok(paths)
}

/// Every routine name the workspace rooted at `orbit_dir` already claims,
/// across both origins, mapped to the file that declares it.
///
/// Names must be unique across every routine source on a host, so a caller
/// that is about to write new definitions uses this to detect a collision
/// before it becomes a load-time error that drops *both* definitions. Files
/// that fail to parse are skipped: [`collect_routines`] treats them as absent,
/// so they claim no name.
pub fn declared_routine_names(orbit_dir: &Path, host_id: &str) -> BTreeMap<String, PathBuf> {
    let routines_dir = orbit_dir.join(ROUTINES_DIR);
    let mut declared = BTreeMap::new();

    collect_declared_names(
        &routines_dir,
        RoutineOrigin::Committed,
        host_id,
        &mut declared,
    );
    collect_declared_names(
        &routines_dir.join(LOCAL_ROUTINES_SUBDIR),
        RoutineOrigin::Local,
        host_id,
        &mut declared,
    );

    declared
}

fn collect_declared_names(
    dir: &Path,
    origin: RoutineOrigin,
    host_id: &str,
    declared: &mut BTreeMap<String, PathBuf>,
) {
    let Ok(paths) = yaml_files_in(dir) else {
        return;
    };
    for path in paths {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let parsed = match origin {
            RoutineOrigin::Committed => parse_routine_yaml(&raw).ok(),
            RoutineOrigin::Local => parse_local_routine_yaml(&raw, host_id).ok(),
        };
        if let Some(definition) = parsed {
            declared.entry(definition.name).or_insert(path);
        }
    }
}

fn load_routine_file(
    path: &Path,
    origin: RoutineOrigin,
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> bool,
    host_id: &str,
) -> Result<LoadedRoutine, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
    // Origin decides the host contract: committed definitions must pin a host,
    // local definitions are implicit to (and may name only) this host.
    let definition = match origin {
        RoutineOrigin::Committed => parse_routine_yaml(&raw).map_err(|error| error.to_string())?,
        RoutineOrigin::Local => {
            parse_local_routine_yaml(&raw, host_id).map_err(|error| error.to_string())?
        }
    };

    // Load-time cron validation: a routine with an unparsable trigger never
    // reaches the due computation.
    if definition.trigger.deliveries_landed.is_none() && definition.trigger.state.is_none() {
        parse_cron(&definition.trigger.cron).map_err(|error| error.to_string())?;
    }

    // Load-time target resolution through the source workspace's catalog,
    // like `target:` steps in JobV2: an unresolvable target is a load error,
    // not a fire-time surprise (ADR-0206).
    let job_name = definition.target.job_name();
    if !catalog(&source.orbit_dir, job_name) {
        return Err(format!(
            "target 'job:{job_name}' does not resolve in workspace '{}': no such job in its catalog",
            source.workspace
        ));
    }

    Ok(LoadedRoutine {
        definition,
        origin,
        source_workspace: source.workspace.clone(),
        source_orbit_dir: source.orbit_dir.clone(),
        path: path.to_path_buf(),
    })
}

/// Names must be unique across every routine source *and origin* on a host; a
/// collision is a load-time error and every colliding definition is treated as
/// absent (fail-closed — firing an arbitrary winner would make behavior depend
/// on iteration order, and a committed and a local definition must never
/// silently shadow one another). Each colliding definition's error names all
/// conflicting sources so both origins are visible.
fn drop_name_collisions(collection: &mut RoutineCollection) {
    // Collect a stable, sorted descriptor of every source per name.
    let mut sources_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for routine in &collection.routines {
        sources_by_name
            .entry(routine.definition.name.clone())
            .or_default()
            .push(format!(
                "{} ({} origin, workspace '{}')",
                routine.path.display(),
                routine.origin.as_str(),
                routine.source_workspace
            ));
    }
    let colliding: BTreeMap<String, Vec<String>> = sources_by_name
        .into_iter()
        .filter_map(|(name, mut sources)| {
            if sources.len() > 1 {
                sources.sort();
                Some((name, sources))
            } else {
                None
            }
        })
        .collect();
    if colliding.is_empty() {
        return;
    }
    let mut kept = Vec::with_capacity(collection.routines.len());
    for routine in collection.routines.drain(..) {
        if let Some(sources) = colliding.get(&routine.definition.name) {
            collection.errors.push(RoutineLoadError {
                source_workspace: routine.source_workspace.clone(),
                path: Some(routine.path.clone()),
                message: format!(
                    "routine name '{}' is defined by more than one source; names must be \
                     unique across committed and local origins on a host — defined at: {}",
                    routine.definition.name,
                    sources.join("; ")
                ),
            });
        } else {
            kept.push(routine);
        }
    }
    collection.routines = kept;
}
