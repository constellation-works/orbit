use std::path::PathBuf;
use std::sync::Arc;

use crate::Store;
use crate::contracts::{
    AuditEventStoreBackend, ExecutorDefStoreBackend, FrictionStoreBackend, InvocationStoreBackend,
    JobRunStoreBackend, PolicyDefStoreBackend, RoutineStoreBackend, SessionLogStoreBackend,
    TaskArtifactStoreBackend, TaskDocumentStoreBackend, TaskHistoryStoreBackend,
    TaskReservationStoreBackend, TaskStoreBackend, ToolStoreBackend, V2AuditStoreBackend,
};
use crate::driver::file::executor_def_store::ExecutorDefFileStore;
use crate::driver::file::policy_def_store::PolicyDefFileStore;
use crate::driver::file::session_log_store::SessionLogStore;
use crate::driver::sqlite::job_run_store::SqliteJobRunStore;
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::repository::friction::FrictionStore;
use crate::repository::layered_policy::LayeredPolicyDefStore;
use crate::repository::sqlite_backends::{
    SqliteAuditEventStoreBackend, SqliteTaskReservationStoreBackend, SqliteToolStoreBackend,
};
use crate::repository::task::{TaskCommitBoundary, TaskV2Store};
use crate::workflow::friction::import_workspace_frictions;

pub struct WorkspaceTaskBackends {
    pub task: Arc<dyn TaskStoreBackend>,
    pub document: Arc<dyn TaskDocumentStoreBackend>,
    pub history: Arc<dyn TaskHistoryStoreBackend>,
    pub artifact: Arc<dyn TaskArtifactStoreBackend>,
}

pub fn workspace_task_backends(
    registry: TaskRegistryStore,
    workspace_id: String,
) -> WorkspaceTaskBackends {
    let store = Arc::new(TaskV2Store::new(registry, workspace_id));
    WorkspaceTaskBackends {
        task: store.clone(),
        document: store.clone(),
        history: store.clone(),
        artifact: store,
    }
}

/// Constructs coordination-only task backends for a logical workspace that
/// has no checkout on this machine. Canonical bundles and registry indexes
/// remain available without a distinct checkout-local store.
pub fn coordination_task_backends(
    registry: TaskRegistryStore,
    workspace_id: String,
) -> WorkspaceTaskBackends {
    let store = Arc::new(TaskV2Store::new(registry, workspace_id));
    WorkspaceTaskBackends {
        task: store.clone(),
        document: store.clone(),
        history: store.clone(),
        artifact: store,
    }
}

pub fn workspace_job_run_store(
    store: Store,
    workspace_id: impl Into<String>,
) -> Arc<dyn JobRunStoreBackend> {
    Arc::new(SqliteJobRunStore::new(store, workspace_id))
}

/// Build the live friction repository after the explicit, idempotent legacy
/// import workflow has committed (or reported an earlier completion).
pub fn workspace_friction_store(
    store: Store,
    workspace_id: impl Into<String>,
    files_root: impl Into<PathBuf>,
) -> Result<Arc<dyn FrictionStoreBackend>, orbit_common::OrbitError> {
    let workspace_id = workspace_id.into();
    let files_root = files_root.into();
    if let Err(error) = import_workspace_frictions(&store, &workspace_id, &files_root) {
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
    Ok(Arc::new(FrictionStore::open(
        store,
        workspace_id,
        files_root,
    )?))
}

pub fn workspace_friction_store_from_path(
    database: &std::path::Path,
    workspace_id: impl Into<String>,
    files_root: impl Into<PathBuf>,
) -> Result<Arc<dyn FrictionStoreBackend>, orbit_common::OrbitError> {
    workspace_friction_store(Store::open(database)?, workspace_id, files_root)
}

/// Prove the store database is open-able *and writable by this binary*.
///
/// Callers use this before work that must write (the pipeline worker's
/// pre-claim pre-flight). A database newer than this binary opens read-only
/// under the forward-compatibility contract (ORB-12434), which is not
/// readiness for those callers — fail here rather than mid-run.
pub fn ensure_sqlite_store_ready(
    database: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    let store = Store::open(database)?;
    if let Some(forward) = store.forward_compatible_open() {
        return Err(orbit_common::OrbitError::Migration(format!(
            "store database '{}' records {} version {} and this orbit binary supports {}, \
             so it opened read-only; work that writes needs a newer orbit",
            database.display(),
            forward.component,
            forward.state_version,
            forward.supported_version
        )));
    }
    Ok(())
}

pub fn global_executor_def_store(root: PathBuf) -> Arc<dyn ExecutorDefStoreBackend> {
    Arc::new(ExecutorDefFileStore::new(root))
}

pub fn workspace_session_log_store(orbit_dir: PathBuf) -> Arc<dyn SessionLogStoreBackend> {
    Arc::new(SessionLogStore::new(orbit_dir))
}

pub fn routine_store(
    database: &std::path::Path,
) -> Result<Arc<dyn RoutineStoreBackend>, orbit_common::OrbitError> {
    Ok(Arc::new(Store::open(database)?))
}

pub fn invocation_store(
    database: &std::path::Path,
) -> Result<Arc<dyn InvocationStoreBackend>, orbit_common::OrbitError> {
    Ok(invocation_store_from_store(Store::open(database)?))
}

/// Compose invocation accounting over an already-opened host store.
pub fn invocation_store_from_store(store: Store) -> Arc<dyn InvocationStoreBackend> {
    Arc::new(store)
}

pub fn v2_audit_store(
    database: &std::path::Path,
) -> Result<Arc<dyn V2AuditStoreBackend>, orbit_common::OrbitError> {
    Ok(v2_audit_store_from_store(Store::open(database)?))
}

/// Compose the v2 audit contract over an already-opened host store.
pub fn v2_audit_store_from_store(store: Store) -> Arc<dyn V2AuditStoreBackend> {
    Arc::new(store)
}

pub fn tool_store_sqlite(store: Store) -> Arc<dyn ToolStoreBackend> {
    Arc::new(SqliteToolStoreBackend { store })
}

pub fn audit_event_store_sqlite(store: Store) -> Arc<dyn AuditEventStoreBackend> {
    Arc::new(SqliteAuditEventStoreBackend { store })
}

pub fn task_reservation_store_sqlite(store: Store) -> Arc<dyn TaskReservationStoreBackend> {
    Arc::new(SqliteTaskReservationStoreBackend {
        store,
        coordination: None,
    })
}

/// One workspace's task backends, reservation store, and the commit boundary
/// they share.
///
/// Returned together on purpose: the boundary only serializes what holds the
/// same instance, so a caller that composed the task backends here must take
/// its reservation store from here too.
pub struct CoordinatedWorkspaceBackends {
    pub task: WorkspaceTaskBackends,
    pub reservation: Arc<dyn TaskReservationStoreBackend>,
    /// The durable commit/recovery authority. Admission publishes a task
    /// transition, its history, a reservation, and dependent coordination rows
    /// through [`TaskCommitBoundary::commit_task_transition`], optionally
    /// inside a [`TaskCommitBoundary::with_admission`] section that also
    /// covers its readiness reads.
    pub commit_boundary: Arc<TaskCommitBoundary>,
}

/// Compose one workspace's task and reservation persistence over a shared
/// durable commit boundary (ORB-12528).
///
/// The difference from [`workspace_task_backends`] plus
/// [`task_reservation_store_sqlite`] is serialization and recovery, not
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
    let task_store = Arc::new(TaskV2Store::with_commit_boundary(
        registry,
        workspace_id,
        Arc::clone(&commit_boundary),
    ));
    Ok(CoordinatedWorkspaceBackends {
        task: WorkspaceTaskBackends {
            task: task_store.clone(),
            document: task_store.clone(),
            history: task_store.clone(),
            artifact: task_store,
        },
        reservation: Arc::new(SqliteTaskReservationStoreBackend {
            store,
            coordination: Some(Arc::clone(&commit_boundary)),
        }),
        commit_boundary,
    })
}

pub fn global_policy_def_store(root: PathBuf) -> Arc<dyn PolicyDefStoreBackend> {
    Arc::new(PolicyDefFileStore::new(root))
}

pub fn workspace_policy_def_store(root: PathBuf) -> Arc<dyn PolicyDefStoreBackend> {
    Arc::new(PolicyDefFileStore::new(root))
}

pub fn layered_policy_def_store(
    workspace: Arc<dyn PolicyDefStoreBackend>,
    global: Arc<dyn PolicyDefStoreBackend>,
) -> Arc<dyn PolicyDefStoreBackend> {
    Arc::new(LayeredPolicyDefStore::new(workspace, global))
}

#[cfg(test)]
#[cfg(test)]
mod tests;

/// Legacy cursor file persistence, retained for rollback compatibility.
pub mod auto_task {
    pub use crate::driver::file::auto_task::{
        CursorSession, cursor_lock_path, cursor_state_path, load_cursor_state, upsert_cursor,
        with_cursor_lock,
    };
}

/// Open automation contracts over the already-configured host store.
pub fn automation_store(
    store: Store,
) -> Result<Arc<dyn crate::contracts::AutomationStoreBackend>, orbit_common::OrbitError> {
    crate::driver::sqlite::automation::initialize(&store)?;
    Ok(Arc::new(store))
}

/// Open before-PR review ledger/certificate contracts over the host store.
pub fn review_store(
    store: Store,
) -> Result<Arc<dyn crate::contracts::ReviewStoreBackend>, orbit_common::OrbitError> {
    crate::driver::sqlite::review::initialize(&store)?;
    Ok(Arc::new(store))
}

/// Open operation-mode grant/ledger contracts over the configured host store.
pub fn operation_store(
    store: Store,
) -> Result<Arc<dyn crate::contracts::OperationStoreBackend>, orbit_common::OrbitError> {
    crate::driver::sqlite::operation::initialize(&store)?;
    Ok(Arc::new(store))
}
