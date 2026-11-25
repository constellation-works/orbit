//! File-store facade: read views (`FileStore::read`) and write scopes
//! (`FileStore::write`) over one family of file-backed records.
//!
//! The file-backed records (executor definitions, policy definitions, the
//! session log and skills) live under directories, not in `orbit.db`, so a
//! [`crate::Store`] transaction cannot cover them. Each family has its own
//! handle, `FileStore<L>`, where `L` is the family's directory layout from
//! `driver/file/`. Records are reached the same way as the SQLite ones: as
//! `snapshot.<record>()` on a [`FileSnapshot`] and `tx.<record>()` on a
//! [`FileTx`], and every writing method takes the `FileTx`.
//!
//! A file write scope is not one atomic transaction across files. Each
//! writing method keeps the guarantee its record had before: an atomic
//! replace of the one file it writes, and, for the session log, the
//! advisory lock held for the whole read-modify-write.

use std::sync::Arc;

use orbit_common::OrbitError;

use crate::driver::file::executor_def_store::ExecutorDefDir;
use crate::driver::file::policy_def_store::PolicyDefDirs;
use crate::driver::file::session_log_store::SessionLogDir;
use crate::driver::file::skill_store::SkillDirs;
use crate::repository::executor_def::ExecutorDefRepository;
use crate::repository::policy_def::PolicyDefRepository;
use crate::repository::session_log::SessionLogRepository;
use crate::repository::skill::SkillRepository;

/// Handle on one family of file-backed records, laid out as `L`.
pub struct FileStore<L> {
    layout: Arc<L>,
}

impl<L> Clone for FileStore<L> {
    fn clone(&self) -> Self {
        Self {
            layout: Arc::clone(&self.layout),
        }
    }
}

impl<L> std::fmt::Debug for FileStore<L>
where
    L: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("layout", &self.layout)
            .finish()
    }
}

impl<L> FileStore<L> {
    /// Wrap a directory layout from `driver/file/`.
    pub fn new(layout: L) -> Self {
        Self {
            layout: Arc::new(layout),
        }
    }

    pub(crate) fn layout(&self) -> &L {
        &self.layout
    }

    /// Open a read view over the records.
    pub fn read(&self) -> FileSnapshot<'_, L> {
        FileSnapshot {
            layout: &self.layout,
        }
    }

    /// Run `op` with write access to the records. This is the only place a
    /// [`FileTx`] is created.
    pub fn write<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(&FileTx<'_, L>) -> Result<T, OrbitError>,
    {
        op(&FileTx {
            layout: &self.layout,
        })
    }
}

/// A read view over one family of file-backed records.
pub struct FileSnapshot<'a, L> {
    layout: &'a L,
}

impl<'a, L> FileSnapshot<'a, L> {
    pub(crate) fn layout(&self) -> &'a L {
        self.layout
    }
}

/// Write access to one family of file-backed records, created only by
/// [`FileStore::write`].
pub struct FileTx<'a, L> {
    layout: &'a L,
}

impl<'a, L> FileTx<'a, L> {
    pub(crate) fn layout(&self) -> &'a L {
        self.layout
    }
}

impl FileSnapshot<'_, ExecutorDefDir> {
    /// Read-only access to executor definitions.
    pub fn executor_def(&self) -> ExecutorDefRepository<'_, Self> {
        ExecutorDefRepository::for_snapshot(self)
    }
}

impl FileTx<'_, ExecutorDefDir> {
    /// Read-write access to executor definitions.
    pub fn executor_def(&self) -> ExecutorDefRepository<'_, Self> {
        ExecutorDefRepository::for_tx(self)
    }
}

impl FileSnapshot<'_, PolicyDefDirs> {
    /// Read-only access to policy definitions.
    pub fn policy_def(&self) -> PolicyDefRepository<'_, Self> {
        PolicyDefRepository::for_snapshot(self)
    }
}

impl FileTx<'_, PolicyDefDirs> {
    /// Read-write access to policy definitions.
    pub fn policy_def(&self) -> PolicyDefRepository<'_, Self> {
        PolicyDefRepository::for_tx(self)
    }
}

impl FileSnapshot<'_, SessionLogDir> {
    /// Read-only access to the workspace session log.
    pub fn session_log(&self) -> SessionLogRepository<'_, Self> {
        SessionLogRepository::for_snapshot(self)
    }
}

impl FileTx<'_, SessionLogDir> {
    /// Read-write access to the workspace session log.
    pub fn session_log(&self) -> SessionLogRepository<'_, Self> {
        SessionLogRepository::for_tx(self)
    }
}

impl FileSnapshot<'_, SkillDirs> {
    /// Read-only access to skills.
    pub fn skill(&self) -> SkillRepository<'_, Self> {
        SkillRepository::for_snapshot(self)
    }
}

impl FileTx<'_, SkillDirs> {
    /// Access to skills inside a write scope.
    pub fn skill(&self) -> SkillRepository<'_, Self> {
        SkillRepository::for_tx(self)
    }
}
