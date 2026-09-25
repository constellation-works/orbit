//! Entering the boundary: construction and journal binding, the ordinary
//! (shared) and admission (exclusive) sections, and per-thread boundary depth.

#[cfg(test)]
use super::BEFORE_ORDINARY_LOCK;
use super::{
    COORDINATION_LOCK_FILE, COORDINATION_LOCK_LABEL, REQUIRED_MARKER_FILE, TaskCommitBoundary,
};
use crate::Store;
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::repository::task::v2_bundle::TaskBundleStoreV2;
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    atomic_write_text, create_private_dir_all, with_exclusive_file_lock, with_shared_file_lock,
};
use std::path::{Path, PathBuf};

thread_local! {
    /// Depth of boundary sections this thread is inside. Recovery must never
    /// run *inside* a commit this thread is performing: that commit's own
    /// journal row is legitimately unsettled.
    static BOUNDARY_DEPTH: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub(super) struct BoundaryDepth;

impl BoundaryDepth {
    pub(super) fn enter(partition: &Path) -> Self {
        BOUNDARY_DEPTH.with(|depth| depth.borrow_mut().push(partition.to_path_buf()));
        Self
    }

    pub(super) fn active(partition: &Path) -> bool {
        BOUNDARY_DEPTH.with(|depth| depth.borrow().iter().any(|held| held == partition))
    }
}

impl Drop for BoundaryDepth {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.with(|depth| {
            depth.borrow_mut().pop();
        });
    }
}

fn host_lock_for_partition(partition: &Path) -> PathBuf {
    // Partitions are tasks/workspaces/<id>. Keep host metadata beside the
    // registry database, outside the directory enumerated as workspaces.
    partition
        .ancestors()
        .nth(2)
        .unwrap_or(partition)
        .join("host-task-commit")
}

impl TaskCommitBoundary {
    pub fn new(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        create_private_dir_all(&partition_dir)
            .map_err(|error| OrbitError::from_write_io(&partition_dir, error))?;
        let boundary = Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
        };
        with_exclusive_file_lock(
            &boundary.lock_target(),
            COORDINATION_LOCK_LABEL,
            || -> Result<(), OrbitError> {
                let marker = boundary.partition_dir.join(REQUIRED_MARKER_FILE);
                if marker.try_exists()? {
                    boundary.verify_journal_binding()?;
                } else {
                    let path = serde_json::to_string(&boundary.store.task_commit_database_path()?)
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    atomic_write_text(&marker, &path)
                        .map_err(|error| OrbitError::from_write_io(&marker, error))?;
                }
                Ok(())
            },
        )?;
        Ok(boundary)
    }

    /// Observation-only handle: no directory creation, lock files, or marker writes.
    pub fn for_observation(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        Ok(Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
        })
    }

    pub(super) fn verify_journal_binding(&self) -> Result<(), OrbitError> {
        let marker = self.partition_dir.join(REQUIRED_MARKER_FILE);
        if marker.try_exists()? {
            let path: PathBuf = serde_json::from_str(&std::fs::read_to_string(marker)?)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if path != self.store.task_commit_database_path()? {
                return Err(OrbitError::Store(
                    "task partition is bound to a different coordination journal".into(),
                ));
            }
        }
        Ok(())
    }

    /// Legacy compositions may serve an uncoordinated partition, but cannot
    /// race or overwrite one that has opted into durable coordination.
    pub(crate) fn enter_uncoordinated<T>(
        registry: &TaskRegistryStore,
        workspace_id: &str,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let partition = registry.workspace_partition_dir(workspace_id)?;
        let host_lock = host_lock_for_partition(&partition);
        with_shared_file_lock(&host_lock, COORDINATION_LOCK_LABEL, || {
            with_shared_file_lock(
                &partition.join(COORDINATION_LOCK_FILE),
                COORDINATION_LOCK_LABEL,
                || {
                    if partition.join(REQUIRED_MARKER_FILE).try_exists()? {
                        return Err(OrbitError::Store(
                            "this task partition requires coordinated backends".into(),
                        ));
                    }
                    op()
                },
            )
        })
    }

    /// The task-store partition this boundary serializes.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Run an ordinary task or reservation operation inside the boundary.
    ///
    /// Shared with every other ordinary participant and excluded by an
    /// admission section. Reads take it too, so a caller cannot observe a
    /// reservation whose task transition is still being applied.
    pub fn enter_ordinary<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        with_shared_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            self.enter_ordinary_locked(op)
        })
    }

    fn enter_ordinary_locked<T>(
        &self,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let mut op = Some(op);
        loop {
            self.recover_if_pending()?;
            #[cfg(test)]
            BEFORE_ORDINARY_LOCK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            let result =
                with_shared_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                    // A commit may have crashed while we waited for this lock.
                    // Drop the shared acquisition before taking recovery exclusive.
                    if !BoundaryDepth::active(&self.partition_dir)
                        && self.pending_marker_path().try_exists()?
                    {
                        return Ok(None);
                    }
                    let _depth = BoundaryDepth::enter(&self.partition_dir);
                    let operation = op.take().ok_or_else(|| {
                        OrbitError::Store("ordinary boundary operation was already consumed".into())
                    })?;
                    operation().map(Some)
                })?;
            if let Some(result) = result {
                return Ok(result);
            }
        }
    }

    /// Hold the boundary exclusively for one admission decision.
    ///
    /// Readiness, dependencies, conflicts, and the commit itself run inside
    /// `op`, so nothing an ordinary write could change moves underneath the
    /// decision. Calling [`Self::commit_task_transition`] inside `op` re-enters
    /// the same acquisition.
    pub fn with_admission<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        with_exclusive_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            with_exclusive_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                self.recover_if_pending()?;
                let _depth = BoundaryDepth::enter(&self.partition_dir);
                op()
            })
        })
    }

    /// Dependent coordination rows published for this partition, by kind.
    ///
    /// Settles an interrupted commit first, so a caller reading back its own
    /// receipts cannot miss one that a crashed commit had already decided.
    pub fn coordination_rows(
        &self,
        kind: &str,
    ) -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.enter_ordinary(|| self.store.task_coordination_rows(&self.workspace_id, kind))
    }

    /// One dependent coordination row by identity, settling an interrupted
    /// commit first like [`Self::coordination_rows`].
    pub fn coordination_row(
        &self,
        kind: &str,
        row_id: &str,
    ) -> Result<Option<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.enter_ordinary(|| {
            self.store
                .task_coordination_row(&self.workspace_id, kind, row_id)
        })
    }

    pub(super) fn host_lock_target(&self) -> PathBuf {
        host_lock_for_partition(&self.partition_dir)
    }

    pub(super) fn lock_target(&self) -> PathBuf {
        self.partition_dir.join(COORDINATION_LOCK_FILE)
    }

    #[cfg(test)]
    pub(crate) fn store_handle(&self) -> &Store {
        &self.store
    }
}
