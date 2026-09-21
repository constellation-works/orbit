//! Routine discovery [ORB-10021]: visit every registered, active owner
//! checkout on this host and load `.orbit/routines/*.yaml` from each —
//! fail-closed per file. An invalid definition becomes a load error and that
//! routine is treated as absent; it never fires with defaults. A definition
//! targeting a job in [`RETIRED_ROUTINE_JOBS`] is skipped as retired instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_types::workflow::RoutineDefinition;

use super::due::parse_cron;

/// Directory under a source workspace's `.orbit/` holding routine YAML files.
pub const ROUTINES_DIR: &str = "routines";

/// Subdirectory of [`ROUTINES_DIR`] that older checkouts used for definitions
/// that were not git-committed. `.orbit/` is now ignored in full, so this is
/// an ordinary subdirectory: files here still load for one release, then the
/// special-case scan will be dropped.
pub const LOCAL_ROUTINES_SUBDIR: &str = "local";

/// Job names a prior release shipped as routine targets that this release no
/// longer provides, each with why the work needs no routine any more.
///
/// A definition targeting one of these is loaded as *retired* rather than
/// failing to load: the file is dead weight until `orbit workspace sync`
/// retires it, not a broken definition worth an error on every clock tick.
/// A job of the same name that the source workspace still defines itself
/// resolves through the catalog first and is never treated as retired.
pub const RETIRED_ROUTINE_JOBS: &[(&str, &str)] = &[
    (
        "auto_task_scheduler_pipeline",
        "auto-task definitions are evaluated directly by every clock tick",
    ),
    (
        "task_triage_pipeline",
        "a failed run leaves its task blocked with the failure attached; re-backlogging is a \
         deliberate human transition",
    ),
];

/// Why a routine targeting `job` is retired, when that job is one a prior
/// release shipped and this one dropped.
pub fn retired_routine_job_reason(job: &str) -> Option<&'static str> {
    RETIRED_ROUTINE_JOBS
        .iter()
        .find(|(name, _)| *name == job)
        .map(|(_, reason)| *reason)
}

/// Compose a retired definition's reason: why the target is gone, then the
/// single step that clears the file. The advice is a parameter because only
/// the layer that owns the managed-routine templates can tell which step
/// applies — see [`sync_retirement_advice`] and [`manual_retirement_advice`].
pub fn retired_routine_reason(job: &str, retirement: &str, advice: &str) -> String {
    format!("target 'job:{job}' is retired in this Orbit ({retirement}); {advice}")
}

/// The step that clears a definition Orbit seeded: synchronization retires a
/// managed routine by content provenance.
pub fn sync_retirement_advice(workspace: &str) -> String {
    format!("run `orbit workspace sync` in workspace '{workspace}' to retire the definition")
}

/// The step that clears a definition Orbit did not write. Synchronization
/// never deletes an operator's own routine, so advertising it would leave the
/// operator running a command that reports `unchanged` forever [DANI-10502].
pub fn manual_retirement_advice(path: &Path) -> String {
    format!(
        "delete '{}' or retarget it at a job this Orbit ships",
        path.display()
    )
}

/// Where a routine definition was found on disk. The directory decides; git
/// status is not consulted. Both locations are evaluated identically.
///
/// `Local` remains only so existing `.orbit/routines/local/` files keep
/// loading for one release. It is not a git-uncommitted origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineOrigin {
    /// A definition under `.orbit/routines/` (excluding `local/`).
    Workspace,
    /// A definition under `.orbit/routines/local/`. Accepted as a plain
    /// subdirectory for one release; not a distinct git origin.
    Local,
}

impl RoutineOrigin {
    /// Stable lowercase label for reporting (`workspace` / `local`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Local => "local",
        }
    }
}

/// One successfully loaded, fully validated routine.
#[derive(Debug, Clone)]
pub struct LoadedRoutine {
    /// The parsed definition.
    pub definition: RoutineDefinition,
    /// Directory the definition was loaded from (`workspace` or `local/`).
    pub origin: RoutineOrigin,
    /// Registry name of the source workspace.
    pub source_workspace: String,
    /// The source workspace's `.orbit` directory (dispatch root).
    pub source_orbit_dir: PathBuf,
    /// Path of the YAML file the definition came from.
    pub path: PathBuf,
}

/// A definition that parsed but targets a job in [`RETIRED_ROUTINE_JOBS`]:
/// skipped this pass, reported so `routine list` and the dashboard can show
/// it as retired rather than as a load error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetiredRoutine {
    /// The routine name the definition declares.
    pub name: String,
    /// Directory the definition was loaded from (`workspace` or `local/`).
    pub origin: RoutineOrigin,
    /// Registry name of the source workspace.
    pub source_workspace: String,
    /// Path of the YAML file the definition came from.
    pub path: PathBuf,
    /// The retired job the definition targets.
    pub job: String,
    /// Human-readable explanation, including the one step that clears the
    /// file. Discovery cannot tell a definition Orbit seeded from one the
    /// operator wrote, so it states the synchronization step and the layer
    /// owning managed-routine provenance narrows it — see
    /// [`retired_routine_reason`].
    pub reason: String,
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

/// Result of resolving a routine target against the source workspace's job
/// catalog. A target can resolve from a healthy layer even when another layer
/// produced a load diagnostic that must still be reported.
#[derive(Debug, Clone, Default)]
pub struct RoutineCatalogLookup {
    /// Whether the requested job name resolves for execution.
    pub resolves: bool,
    /// A catalog diagnostic to report alongside a successfully resolved target.
    pub error: Option<String>,
}

/// Result of one discovery pass across all routine sources.
#[derive(Debug, Default)]
pub struct RoutineCollection {
    /// Valid routines, in stable (workspace, filename) order.
    pub routines: Vec<LoadedRoutine>,
    /// Definitions targeting a retired job, skipped without an error.
    pub retired: Vec<RetiredRoutine>,
    /// Routine/source load failures and catalog diagnostics reported during
    /// discovery.
    pub errors: Vec<RoutineLoadError>,
}

/// Explicit source description prepared by Core/registry composition.
#[derive(Debug, Clone)]
pub struct RoutineSource {
    pub workspace: String,
    pub orbit_dir: PathBuf,
}

/// Load routines from every source workspace among `workspaces` (the same
/// runtimes are later used for dispatch), from both origins. Cross-origin
/// name collisions are load-time errors: every colliding definition is
/// dropped and each conflicting source is named.
pub fn collect_routines(
    workspaces: &[RoutineSource],
    catalog: &dyn Fn(&Path, &str) -> RoutineCatalogLookup,
) -> RoutineCollection {
    let mut collection = RoutineCollection::default();

    for source in workspaces {
        load_source_workspace(source, catalog, &mut collection);
    }

    drop_name_collisions(&mut collection);
    collection
}

fn load_source_workspace(
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> RoutineCatalogLookup,
    collection: &mut RoutineCollection,
) {
    let mut catalog_errors = std::collections::BTreeSet::new();

    // Top-level YAML files. The `local/` subdirectory is a directory (never a
    // file) so it is skipped here and scanned separately for one release.
    match yaml_files_in(&source.orbit_dir, RoutineOrigin::Workspace) {
        Ok(paths) => load_origin_files(
            paths,
            RoutineOrigin::Workspace,
            source,
            catalog,
            &mut catalog_errors,
            collection,
        ),
        // A source with no routines directory is simply an empty source.
        Err(RoutinesDirectoryError::Missing) => return,
        Err(error) => {
            collection.errors.push(RoutineLoadError {
                source_workspace: source.workspace.clone(),
                path: Some(source.orbit_dir.join(ROUTINES_DIR)),
                message: format!("failed to list routines directory: {error}"),
            });
            return;
        }
    }

    // `.orbit/routines/local/` remains loadable as a plain subdirectory.
    match yaml_files_in(&source.orbit_dir, RoutineOrigin::Local) {
        Ok(paths) => {
            let local_dir = source
                .orbit_dir
                .join(ROUTINES_DIR)
                .join(LOCAL_ROUTINES_SUBDIR);
            tracing::info!(
                workspace = %source.workspace,
                path = %local_dir.display(),
                ".orbit/routines/local/ is no longer a distinct origin; definitions there load as ordinary workspace routines and the subdirectory will be dropped as a special case in a later release"
            );
            load_origin_files(
                paths,
                RoutineOrigin::Local,
                source,
                catalog,
                &mut catalog_errors,
                collection,
            );
        }
        Err(RoutinesDirectoryError::Missing) => {}
        Err(error) => {
            collection.errors.push(RoutineLoadError {
                source_workspace: source.workspace.clone(),
                path: Some(
                    source
                        .orbit_dir
                        .join(ROUTINES_DIR)
                        .join(LOCAL_ROUTINES_SUBDIR),
                ),
                message: format!("failed to list routines directory: {error}"),
            });
        }
    }
}

/// Load every listed YAML file under `origin`. Only regular files are
/// considered, so a scan of `.orbit/routines/` never treats the `local/`
/// subdirectory as a definition.
fn load_origin_files(
    paths: Vec<PathBuf>,
    origin: RoutineOrigin,
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> RoutineCatalogLookup,
    catalog_errors: &mut std::collections::BTreeSet<String>,
    collection: &mut RoutineCollection,
) {
    for path in paths {
        match load_routine_file(&path, origin, source, catalog) {
            Ok(RoutineLoadOutcome {
                routine,
                catalog_error,
            }) => {
                if let Some(error) = catalog_error {
                    let message = format!(
                        "failed to load job catalog for workspace '{}': {error}",
                        source.workspace
                    );
                    if catalog_errors.insert(message.clone()) {
                        collection.errors.push(RoutineLoadError {
                            source_workspace: source.workspace.clone(),
                            path: None,
                            message,
                        });
                    }
                }
                match routine {
                    RoutineLoad::Active(routine) => collection.routines.push(*routine),
                    RoutineLoad::Retired(routine) => collection.retired.push(routine),
                }
            }
            Err(message) => collection.errors.push(RoutineLoadError {
                source_workspace: source.workspace.clone(),
                path: Some(path),
                message,
            }),
        }
    }
}

#[derive(Debug)]
enum RoutinesDirectoryError {
    Missing,
    Invalid(String),
    List(std::io::Error),
}

impl fmt::Display for RoutinesDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("routines directory is missing"),
            Self::Invalid(message) => formatter.write_str(message),
            Self::List(error) => write!(formatter, "{error}"),
        }
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a SafeAccessCheck
/// on the receiver. Call this after reconstructing a routines origin so later
/// `read_dir` sinks only see a prefix-checked value.
fn routines_origin_dir_is_contained(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

fn resolve_exact_child_dir(
    parent: &Path,
    name: &str,
    child_label: &str,
) -> Result<PathBuf, RoutinesDirectoryError> {
    let expected_dir = parent.join(name);
    if !routines_origin_dir_is_contained(&expected_dir, parent) {
        return Err(RoutinesDirectoryError::Invalid(format!(
            "{child_label} directory escapes {}",
            parent.display()
        )));
    }
    let canonical_dir = match fs::canonicalize(&expected_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RoutinesDirectoryError::Missing);
        }
        Err(error) => {
            return Err(RoutinesDirectoryError::Invalid(format!(
                "failed to resolve {child_label} directory {}: {error}",
                expected_dir.display()
            )));
        }
    };
    if canonical_dir != expected_dir || !canonical_dir.is_dir() {
        return Err(RoutinesDirectoryError::Invalid(format!(
            "{child_label} directory must be a regular directory directly under {}",
            parent.display()
        )));
    }
    Ok(canonical_dir)
}

/// Resolve a routines origin directory before it reaches `read_dir`.
///
/// The runtime supplies the Orbit root, while the directory names are fixed by
/// this module. Canonicalizing both components and requiring the exact direct
/// child prevents a symlinked `routines/` or `routines/local/` directory from
/// redirecting a scan.
fn validated_routines_origin_dir(
    orbit_dir: &Path,
    origin: RoutineOrigin,
) -> Result<PathBuf, RoutinesDirectoryError> {
    let canonical_orbit_dir = match fs::canonicalize(orbit_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(RoutinesDirectoryError::Missing);
        }
        Err(error) => {
            return Err(RoutinesDirectoryError::Invalid(format!(
                "failed to resolve Orbit directory {}: {error}",
                orbit_dir.display()
            )));
        }
    };
    let canonical_routines_dir =
        resolve_exact_child_dir(&canonical_orbit_dir, ROUTINES_DIR, "routines")?;
    match origin {
        RoutineOrigin::Workspace => Ok(canonical_routines_dir),
        RoutineOrigin::Local => resolve_exact_child_dir(
            &canonical_routines_dir,
            LOCAL_ROUTINES_SUBDIR,
            "local routines",
        ),
    }
}

/// Regular `*.yaml` / `*.yml` files directly in a validated origin directory,
/// in stable filename order. Subdirectories (e.g. `local/` under the committed
/// scan) are skipped. Directory listing never consumes the caller `orbit_dir`.
fn yaml_files_in(
    orbit_dir: &Path,
    origin: RoutineOrigin,
) -> Result<Vec<PathBuf>, RoutinesDirectoryError> {
    let dir = validated_routines_origin_dir(orbit_dir, origin)?;
    let mut paths = Vec::new();
    let entries = fs::read_dir(&dir).map_err(RoutinesDirectoryError::List)?;
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = dir.join(entry.file_name());
        if !routines_origin_dir_is_contained(&path, &dir) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml"))
        {
            paths.push(path);
        }
    }
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
pub fn declared_routine_names(orbit_dir: &Path) -> BTreeMap<String, PathBuf> {
    let mut declared = BTreeMap::new();

    collect_declared_names(orbit_dir, RoutineOrigin::Workspace, &mut declared);
    collect_declared_names(orbit_dir, RoutineOrigin::Local, &mut declared);

    declared
}

fn collect_declared_names(
    orbit_dir: &Path,
    origin: RoutineOrigin,
    declared: &mut BTreeMap<String, PathBuf>,
) {
    let Ok(paths) = yaml_files_in(orbit_dir, origin) else {
        return;
    };
    for path in paths {
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(definition) = parse_routine_yaml(&raw) {
            declared.entry(definition.name).or_insert(path);
        }
    }
}

/// A definition that parsed: either evaluable this pass, or retired.
enum RoutineLoad {
    Active(Box<LoadedRoutine>),
    Retired(RetiredRoutine),
}

fn load_routine_file(
    path: &Path,
    origin: RoutineOrigin,
    source: &RoutineSource,
    catalog: &dyn Fn(&Path, &str) -> RoutineCatalogLookup,
) -> Result<RoutineLoadOutcome, String> {
    let raw = fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
    let definition = parse_routine_yaml(&raw).map_err(|error| error.to_string())?;

    let job_name = definition.target.job_name();
    let catalog_lookup = catalog(&source.orbit_dir, job_name);
    // A routine whose target is a job a prior release shipped and this one
    // dropped is retired, not broken: it is skipped and reported as such so
    // the clock tick does not log the same load error forever. The catalog
    // wins when the workspace defines a job of that name itself.
    if !catalog_lookup.resolves
        && catalog_lookup.error.is_none()
        && let Some(reason) = retired_routine_job_reason(job_name)
    {
        return Ok(RoutineLoadOutcome {
            routine: RoutineLoad::Retired(RetiredRoutine {
                name: definition.name,
                origin,
                source_workspace: source.workspace.clone(),
                path: path.to_path_buf(),
                job: job_name.to_string(),
                reason: retired_routine_reason(
                    job_name,
                    reason,
                    &sync_retirement_advice(&source.workspace),
                ),
            }),
            catalog_error: None,
        });
    }

    if !catalog_lookup.resolves {
        if let Some(error) = catalog_lookup.error {
            return Err(format!(
                "failed to load job catalog for workspace '{}': {error}",
                source.workspace
            ));
        }
        return Err(format!(
            "target 'job:{job_name}' does not resolve in workspace '{}': no such job in its catalog",
            source.workspace
        ));
    }

    // [ORB-12236] Definitions carry no host pin. A file that still has one
    // loads and is evaluated here; the warning names it so the key can be
    // dropped before the next release rejects it.
    if definition.has_legacy_host_pin() {
        tracing::warn!(
            target: "orbit.routines",
            path = %path.display(),
            routine = %definition.name,
            "routine still declares the retired `hosts:` key; it is ignored and the routine \
             is evaluated on this host — remove the key from the definition",
        );
    }

    // Load-time cron validation: a routine with an unparsable trigger never
    // reaches the due computation.
    if definition.trigger.deliveries_landed.is_none() && definition.trigger.state.is_none() {
        parse_cron(&definition.trigger.cron).map_err(|error| error.to_string())?;
    }

    Ok(RoutineLoadOutcome {
        routine: RoutineLoad::Active(Box::new(LoadedRoutine {
            definition,
            origin,
            source_workspace: source.workspace.clone(),
            source_orbit_dir: source.orbit_dir.clone(),
            path: path.to_path_buf(),
        })),
        catalog_error: catalog_lookup.error,
    })
}

struct RoutineLoadOutcome {
    routine: RoutineLoad,
    catalog_error: Option<String>,
}

/// Names must be unique across every routine source on a host; a collision is
/// a load-time error and every colliding definition is treated as absent
/// (fail-closed — firing an arbitrary winner would make behavior depend on
/// iteration order). Each colliding definition's error names all conflicting
/// sources.
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
