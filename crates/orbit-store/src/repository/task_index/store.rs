use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;

use super::change_stamps::TaskChangeStamps;
use super::index::TaskIndexSnapshots;
use crate::driver::sqlite::host::read_pool::ReadGuard;
use crate::driver::sqlite::task_index::TaskIndexDb;

/// The task registry: the repository over `tasks/index.sqlite`.
///
/// The driver's `TaskIndexDb` opens the file and owns its connections; this
/// type holds the SQL and row mapping, split by concern across this module's
/// siblings, and reaches the file only through `TaskIndexDb::read` and
/// `TaskIndexDb::write` (derived caches use `TaskIndexDb::cache_write`).
#[derive(Clone)]
pub struct TaskRegistryStore {
    pub(super) db: TaskIndexDb,
    pub(super) index_snapshots: Arc<TaskIndexSnapshots>,
    pub(super) change_stamps: Arc<TaskChangeStamps>,
}

impl TaskRegistryStore {
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        TaskIndexDb::open(path).map(Self::over)
    }

    /// Open an existing registry without creating files, applying schema, or
    /// taking a writer connection. Used by a differing-generation read-only join.
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        TaskIndexDb::open_read_only(path).map(Self::over)
    }

    fn over(db: TaskIndexDb) -> Self {
        Self {
            db,
            index_snapshots: Arc::default(),
            change_stamps: Arc::default(),
        }
    }

    /// Whether this registry was opened for observation only.
    pub(crate) fn is_read_only(&self) -> Result<bool, OrbitError> {
        self.db.is_read_only()
    }

    /// Check out a read connection from the task index's read path.
    pub(super) fn read(&self) -> Result<ReadGuard<'_>, OrbitError> {
        self.db.read()
    }

    /// Root directory that holds per-workspace canonical bundle trees
    /// (`<global>/tasks/workspaces`). Used by task-migration tooling to locate
    /// and enumerate on-disk bundles for a workspace.
    pub(crate) fn workspaces_dir(&self) -> &Path {
        self.db.workspaces_dir()
    }
}
