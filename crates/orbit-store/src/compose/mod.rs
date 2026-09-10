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
use crate::repository::task::TaskV2Store;
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

pub fn ensure_sqlite_store_ready(
    database: &std::path::Path,
) -> Result<(), orbit_common::OrbitError> {
    drop(Store::open(database)?);
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
    Arc::new(SqliteTaskReservationStoreBackend { store })
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
