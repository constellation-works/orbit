//! Composition constructors that assemble driver, repository, and migration store handles; imports `contract`, `driver`, `repository`, `migrate`, and `orbit-common`.

mod compatibility;
/// Composition for workspace-local auto-task cursors.
pub mod auto_task {
    pub use crate::driver::file::auto_task::cursor_state_path;
    use crate::file_store::AutoTaskStore;
    pub use crate::repository::auto_task::AutoTaskWriteRepository;
    use std::path::Path;

    /// Bind a cursor store to its workspace state file.
    pub fn cursor_store(path: &Path) -> AutoTaskStore {
        AutoTaskStore::new(path.to_path_buf())
    }
}

use crate::Store;
use crate::boundary::{TaskCommitBoundary, TaskCoordination, TaskV2Store};
use crate::driver::file::executor_def_store::ExecutorDefDir;
use crate::driver::file::policy_def_store::PolicyDefDirs;
use crate::driver::file::session_log_store::SessionLogDir;
use crate::file_store::FileStore;
use crate::migrate::friction::import_workspace_frictions;
use crate::repository::task_index::TaskRegistryStore;
use crate::{
    TaskArtifactRepository, TaskDocumentRepository, TaskHistoryRepository,
    TaskReservationRepository,
};
pub use compatibility::compiled_compatibility;
use std::path::PathBuf;
use std::sync::Arc;

pub struct WorkspaceTaskBackends {
    pub task: Arc<TaskV2Store>,
    pub coordination: TaskCoordination,
    pub document: TaskDocumentRepository,
    pub history: TaskHistoryRepository,
    pub artifact: TaskArtifactRepository,
}

impl WorkspaceTaskBackends {
    fn new(store: TaskV2Store, coordination: TaskCoordination) -> Self {
        let task = Arc::new(store);
        let document = TaskDocumentRepository {
            store: task.clone(),
        };
        let history = TaskHistoryRepository {
            store: task.clone(),
        };
        let artifact = TaskArtifactRepository {
            store: task.clone(),
        };
        Self {
            task,
            coordination,
            document,
            history,
            artifact,
        }
    }
}

/// One workspace's task backends, reservation store, and the commit boundary
/// they share.
///
/// Returned together on purpose: the boundary only serializes what holds the
/// same instance, so a caller that composed the task backends here must take
/// its reservation store from here too.
pub struct CoordinatedWorkspaceBackends {
    pub task: WorkspaceTaskBackends,
    pub reservation: TaskReservationRepository,
    /// The durable commit/recovery authority. Admission publishes a task
    /// transition, its history, a reservation, and dependent coordination rows
    /// through [`TaskCommitBoundary::commit_task_transition`], optionally
    /// inside a [`TaskCommitBoundary::with_admission`] section that also
    /// covers its readiness reads.
    pub commit_boundary: Arc<TaskCommitBoundary>,
}

pub fn workspace_task_backends(
    registry: TaskRegistryStore,
    workspace_id: String,
) -> WorkspaceTaskBackends {
    WorkspaceTaskBackends::new(
        TaskV2Store::new(registry, workspace_id),
        TaskCoordination::default(),
    )
}

/// Run the explicit, idempotent legacy friction import. A read-only store
/// skips the import; the next writable open performs it.
pub fn prepare_workspace_frictions(
    store: &Store,
    workspace_id: &str,
    files_root: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    // A store that cannot write can never persist the import, so attempting it
    // would only repeat a doomed write transaction (and its warning) on every
    // open. The next writable open imports and persists it.
    if !store.is_read_only()
        && let Err(error) = import_workspace_frictions(store, workspace_id, files_root)
    {
        if error.is_readonly_or_access_failure() {
            orbit_common::tracing::warn!(
                target: "orbit.store.friction",
                workspace_id,
                root = %files_root.display(),
                error = %error,
                "skipped incidental legacy-friction import persistence"
            );
        } else {
            return Err(error);
        }
    }
    Ok(())
}

/// Prove the store database is open-able *and writable by this binary*.
///
/// Callers use this before work that must write (the pipeline worker's
/// pre-claim pre-flight). A database newer than this binary opens read-only
/// under the forward-compatibility contract (ORB-12434), which is not
/// readiness for those callers — fail here rather than mid-run.
/// Supported schemas must also pass a write-lock probe with rollback, so
/// observational read-only opens cannot pass readiness.
// A readiness probe opens its own handle; it hands none out.
#[allow(clippy::disallowed_methods)]
pub fn ensure_sqlite_store_ready(
    database: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    let store = Store::open(database)?;
    if let Some(forward) = store
        .forward_compatible_open()
        .filter(|forward| !forward.writable)
    {
        return Err(orbit_common::OrbitError::Migration(format!(
            "store database '{}' records {} version {} and this orbit binary supports {}, \
             so it opened read-only; work that writes needs a newer orbit",
            database.display(),
            forward.component,
            forward.state_version,
            forward.supported_version
        )));
    }
    store.check_writable()
}

/// The executor definitions under `root` (the global `executors/` directory).
pub fn global_executor_def_store(root: PathBuf) -> FileStore<ExecutorDefDir> {
    FileStore::new(ExecutorDefDir::new(root))
}

/// The session log of the workspace whose `.orbit` directory is `orbit_dir`.
pub fn workspace_session_log_store(orbit_dir: PathBuf) -> FileStore<SessionLogDir> {
    FileStore::new(SessionLogDir::new(orbit_dir))
}

/// Compose one workspace's task and reservation persistence over an
/// observation-only commit boundary.
///
/// Reads the same partition state as [`workspace_coordinated_backends`], but
/// opening it initializes nothing on a settled partition: no partition
/// directory, lock file, or journal marker. When an unsettled journal row's
/// lease nobody holds, a writable handle replays the decision before
/// exposure and a read-only handle refuses with a recovery-required error;
/// neither exposes unapplied sidecars as settled. Used by a
/// read-only join of a foreign generation, a read-only command whose partition
/// does not exist yet, and any open of an existing partition on storage the
/// process cannot write.
pub fn workspace_observational_backends(
    registry: TaskRegistryStore,
    workspace_id: String,
    store: Store,
) -> Result<CoordinatedWorkspaceBackends, orbit_common::OrbitError> {
    let commit_boundary = Arc::new(TaskCommitBoundary::for_observation(
        store.clone(),
        registry.clone(),
        workspace_id.clone(),
    )?);
    // The task store joins the same observation handle: a partition that has
    // activated coordination refuses the legacy uncoordinated store, and every
    // partition written since the boundary shipped has.
    let task =
        TaskV2Store::with_commit_boundary(registry, workspace_id, Arc::clone(&commit_boundary));
    Ok(CoordinatedWorkspaceBackends {
        task: WorkspaceTaskBackends::new(task, TaskCoordination::new(Arc::clone(&commit_boundary))),
        reservation: TaskReservationRepository {
            store,
            coordination: Arc::clone(&commit_boundary),
        },
        commit_boundary,
    })
}

/// Compose one workspace's task and reservation persistence over a shared
/// durable commit boundary (ORB-12528).
///
/// The difference from legacy uncoordinated task composition is serialization and recovery, not
/// storage layout: bundles, registry rows, and reservation rows are unchanged,
/// and every existing API behaves as before. What is added is that ordinary
/// task and reservation mutations run inside the boundary, reads settle an
/// interrupted commit before exposing state, and an admission decision can
/// read readiness and publish its transition plus reservation as one durable
/// outcome.
///
/// `store` must be the database that holds this host's reservations.
pub fn workspace_coordinated_backends(
    registry: TaskRegistryStore,
    workspace_id: String,
    store: Store,
) -> Result<CoordinatedWorkspaceBackends, orbit_common::OrbitError> {
    let commit_boundary = Arc::new(TaskCommitBoundary::new(
        store.clone(),
        registry.clone(),
        workspace_id.clone(),
    )?);
    let task =
        TaskV2Store::with_commit_boundary(registry, workspace_id, Arc::clone(&commit_boundary));
    Ok(CoordinatedWorkspaceBackends {
        task: WorkspaceTaskBackends::new(task, TaskCoordination::new(Arc::clone(&commit_boundary))),
        reservation: TaskReservationRepository {
            store,
            coordination: Arc::clone(&commit_boundary),
        },
        commit_boundary,
    })
}

/// The policy definitions under the global `policies/` directory `root`.
pub fn global_policy_def_store(root: PathBuf) -> FileStore<PolicyDefDirs> {
    FileStore::new(PolicyDefDirs::single(root))
}

/// The policy definitions under a workspace's `policies/` directory `root`
/// alone.
pub fn workspace_policy_def_store(root: PathBuf) -> FileStore<PolicyDefDirs> {
    FileStore::new(PolicyDefDirs::single(root))
}

/// The policy definitions of `workspace`, layered over those of `global`;
/// writes go to the workspace directory. Each side contributes its own
/// (primary) directory.
pub fn layered_policy_def_store(
    workspace: FileStore<PolicyDefDirs>,
    global: FileStore<PolicyDefDirs>,
) -> FileStore<PolicyDefDirs> {
    FileStore::new(PolicyDefDirs::layered(
        workspace.layout().primary.clone(),
        global.layout().primary.clone(),
    ))
}
