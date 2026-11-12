//! Registry composition over Core's scheduler kernels: which checkouts this
//! machine evaluates schedules for, and who this machine is.
//!
//! Both come exclusively from the global `config.toml` `[machine]` table and
//! `workspaces.json`.

use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::routines::{
    DiscoveredWorkspaces, RoutineLoadError, RoutineMachineIdentity, RoutineStatusReport,
    RoutineWorkspaceProvider, SweepOptions, SweepOutcome,
};
use orbit_types::workspace::{WorkspaceCheckoutRole, WorkspaceStatus};

use orbit_registry::machine_identity::{MachineIdentity, load_machine_identity};
use orbit_registry::workspace_registry;

use crate::registry_runtime::RegisteredRuntimeFactory;

struct RegistryRoutineEnvironment {
    identity: MachineIdentity,
    /// Registered workspace this pass is restricted to, resolved from the
    /// caller's `--workspace` selector. `None` visits every local workspace.
    workspace_filter: Option<String>,
    read_only: bool,
}

impl RegistryRoutineEnvironment {
    /// An unknown, unregistered, or inactive selector fails closed here,
    /// before the sweep touches any scheduler state: an operator asking for
    /// one workspace must never silently get the whole machine.
    fn load(global_root: &Path, workspace_selector: Option<&str>) -> Result<Self, OrbitError> {
        let workspace_filter = workspace_selector
            .map(|selector| {
                RegisteredRuntimeFactory::resolve_workspace_selector(global_root, selector)
                    .map(|selected| selected.workspace.id)
            })
            .transpose()?;
        Ok(Self {
            identity: load_machine_identity(global_root)?,
            workspace_filter,
            read_only: false,
        })
    }

    fn local_machine(&self) -> RoutineMachineIdentity {
        RoutineMachineIdentity {
            machine_id: self.identity.id.clone(),
            machine_name: self.identity.name.clone(),
        }
    }
}

impl RoutineWorkspaceProvider for RegistryRoutineEnvironment {
    fn discover_workspaces(&self, global_root: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        if self.read_only {
            discover_registered_workspaces_until(
                global_root,
                self.workspace_filter.as_deref(),
                None,
                true,
            )
        } else {
            discover_registered_workspaces(global_root, self.workspace_filter.as_deref())
        }
    }

    fn discover_workspaces_until(
        &self,
        global_root: &Path,
        deadline: Instant,
    ) -> Result<DiscoveredWorkspaces, OrbitError> {
        discover_registered_workspaces_until(
            global_root,
            self.workspace_filter.as_deref(),
            Some(deadline),
            self.read_only,
        )
    }
}

/// Discover the checkouts this machine evaluates schedules for, optionally
/// restricted to one registered workspace id: every active checkout with a
/// `.orbit/` directory. Registration is the whole opt-in [ORB-12236]. A
/// replica cannot write the owner's coordination store, so it is opened apart:
/// the sweep delivers the pull settlements its follower drains recorded
/// [ORB-13892] and fires only its host-local worktree GC and store GC
/// routines, never task minting, shipping or recovery [ORB-14173]
/// [ORB-16354]. The provider delegates here so this production path can be
/// exercised with an explicit global root.
pub(crate) fn discover_registered_workspaces(
    global_root: &Path,
    workspace_filter: Option<&str>,
) -> Result<DiscoveredWorkspaces, OrbitError> {
    discover_registered_workspaces_until(global_root, workspace_filter, None, false)
}

fn discover_registered_workspaces_until(
    global_root: &Path,
    workspace_filter: Option<&str>,
    deadline: Option<Instant>,
    read_only: bool,
) -> Result<DiscoveredWorkspaces, OrbitError> {
    let registry_path = workspace_registry::registry_path_for(global_root);
    let registry = workspace_registry::load_validated_registry_from(
        &registry_path,
        if read_only {
            workspace_registry::WorkspaceValidationMode::ReadOnly
        } else {
            workspace_registry::WorkspaceValidationMode::Persist
        },
    )?;

    let open = if read_only {
        RegisteredRuntimeFactory::open_registered_checkout_observational
    } else {
        RegisteredRuntimeFactory::open_registered_checkout
    };
    let mut discovered = DiscoveredWorkspaces::default();
    for (workspace, checkout) in workspace_registry::local_workspaces(&registry) {
        if workspace.status != WorkspaceStatus::Active || !checkout.orbit_dir.exists() {
            continue;
        }
        if workspace_filter.is_some_and(|selected| selected != workspace.id) {
            continue;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            discovered.skipped_workspaces.push(workspace.name.clone());
            continue;
        }
        if checkout.role == Some(WorkspaceCheckoutRole::Replica) {
            // A replica that cannot be opened only defers its settlement
            // delivery and local GC, so it never reads as a broken workspace.
            match open(global_root, workspace, checkout) {
                Ok(runtime) => discovered.replicas.push((workspace.clone(), runtime)),
                Err(error) => tracing::warn!(
                    target: "orbit.cmd.sweep",
                    workspace = %workspace.name,
                    %error,
                    "replica checkout could not be opened; its pull settlements wait",
                ),
            }
            continue;
        }
        match open(global_root, workspace, checkout) {
            Ok(runtime) => discovered.entries.push((workspace.clone(), runtime)),
            Err(error) => discovered.errors.push(RoutineLoadError {
                source_workspace: workspace.name.clone(),
                path: Some(checkout.orbit_dir.clone()),
                message: format!("failed to open workspace runtime: {error}"),
            }),
        }
    }
    Ok(discovered)
}

pub fn routine_statuses(global_root: &Path) -> Result<RoutineStatusReport, OrbitError> {
    routine_statuses_with_workspace_filter(global_root, None)
}

/// Files and definition directories whose changes invalidate host routine
/// status at once: definitions, configuration and the workspace registry,
/// which change only when an operator edits them. The churning stores are not
/// here: the task registry database is in [`routine_status_task_store_paths`]
/// and the shared audit database, which takes writes from every audit event on
/// the host, is read through [`routine_status_store_fingerprint`].
pub fn routine_status_input_paths(
    global_root: &Path,
) -> Result<Vec<std::path::PathBuf>, OrbitError> {
    let mut paths = vec![workspace_registry::registry_path_for(global_root)];
    let registry = workspace_registry::load_registry_from_read_only(&paths[0])?.registry;
    let roots = std::iter::once(global_root.to_path_buf()).chain(
        workspace_registry::local_workspaces(&registry)
            .map(|(_, checkout)| checkout.orbit_dir.clone()),
    );
    for root in roots {
        paths.push(root.join("config.toml"));
        paths.push(root.join("config.yaml"));
        for directory in [
            "routines",
            "resources/jobs",
            "resources/activities",
            "auto_tasks",
        ] {
            paths.push(root.join(directory));
        }
        // Persistence paths derive from the two roots alone, never from the
        // config document, so the memo's input set needs no config load per
        // workspace (every workspace resolves the same global store paths).
        // A workspace whose config does not load is reported by status as a
        // load error; its config files above still invalidate once fixed.
        let persistence = orbit_config::PersistenceConfig::default_for_roots(global_root, &root);
        paths.push(persistence.job_dir);
        paths.push(persistence.activity_dir);
    }
    paths.push(global_root.join("clock.toml"));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The task registry database and its WAL, whose stamps move with committed
/// task changes (a commit need not touch the main database until a
/// checkpoint). Routine status reads task state, but a live drain writes it
/// constantly, so a caller treats these stamps as a measure of how stale a
/// served status is rather than as an immediate invalidation.
pub fn routine_status_task_store_paths(global_root: &Path) -> [std::path::PathBuf; 2] {
    let database = orbit_store::maintenance::task_registry::task_registry_path(global_root);
    let wal = database.with_file_name(format!(
        "{}-wal",
        database.file_name().unwrap_or_default().to_string_lossy()
    ));
    [wal, database]
}

/// Change tokens over the shared audit database's routine rows, split by how
/// a caller should react to them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoutineStoreFingerprint {
    /// Routine fires, cursors, pauses and automation consumers: rare writes an
    /// operator acts on, so a caller invalidates on them at once.
    pub routines: String,
    /// Job runs, the fire record of state-, delivery- and condition-triggered
    /// routines. A live drain moves this with every run it starts or finishes,
    /// so a caller may serve the previous status for its freshness bound.
    pub job_runs: String,
}

/// Change tokens over the rows (routine fires, cursors and pauses, job runs and
/// automation consumers) that host routine status reads from the global store,
/// `None` before that database exists. Best effort like the file stamps: a
/// database that cannot be read yields `None`, and the caller's freshness bound
/// still limits how long a change can hide.
pub fn routine_status_store_fingerprint(global_root: &Path) -> Option<RoutineStoreFingerprint> {
    let database =
        orbit_config::PersistenceConfig::default_for_roots(global_root, global_root).audit_db;
    if !database.exists() {
        return None;
    }
    orbit_store::Store::open_read_only(&database)
        .and_then(|store| store.routine_state_fingerprint())
        .ok()
        .map(|(routines, job_runs)| RoutineStoreFingerprint { routines, job_runs })
}

/// Read host routine status from one registered workspace.
pub fn routine_statuses_for_workspace(
    global_root: &Path,
    workspace_selector: &str,
) -> Result<RoutineStatusReport, OrbitError> {
    routine_statuses_with_workspace_filter(global_root, Some(workspace_selector))
}

fn routine_statuses_with_workspace_filter(
    global_root: &Path,
    workspace_selector: Option<&str>,
) -> Result<RoutineStatusReport, OrbitError> {
    let started = Instant::now();
    let mut environment = RegistryRoutineEnvironment::load(global_root, workspace_selector)?;
    environment.read_only = true;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "routine status environment loaded"
    );
    orbit_core::application::routines::routine_statuses_with_providers(
        global_root,
        environment.local_machine(),
        &environment,
        Utc::now(),
    )
}

/// Run one sweep pass over this host's registered workspaces, or only the one
/// named by `workspace_selector`.
pub fn run_sweep(
    options: SweepOptions,
    workspace_selector: Option<&str>,
) -> Result<SweepOutcome, OrbitError> {
    let global_root = workspace_registry::global_orbit_dir()?;
    let environment = RegistryRoutineEnvironment::load(&global_root, workspace_selector)?;
    orbit_core::application::routines::run_sweep_with_providers(
        options,
        environment.local_machine(),
        &environment,
    )
}

/// As [`run_sweep`], against an explicit global root.
pub fn run_sweep_at(
    global_root: &Path,
    options: SweepOptions,
    workspace_selector: Option<&str>,
) -> Result<SweepOutcome, OrbitError> {
    let environment = RegistryRoutineEnvironment::load(global_root, workspace_selector)?;
    orbit_core::application::routines::run_sweep_at_with_providers(
        global_root,
        options,
        environment.local_machine(),
        &environment,
    )
}
