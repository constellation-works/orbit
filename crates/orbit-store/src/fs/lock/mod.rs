//! Store-facing adapters for Orbit's common bounded advisory lock.
//!
//! Task bundles and independent store locks use the same deadline, holder
//! metadata, and stable-file behavior. The implementation lives in
//! `orbit-common` so lower-level persistence helpers never depend upward on
//! the store crate.

use orbit_common::OrbitError;
use std::path::Path;

pub use orbit_common::fs::io::FileLockHolderInfo as LockHolderInfo;
pub(crate) use orbit_common::fs::io::{FileLockGuard, FileLockOptions as LockOptions};
use orbit_common::fs::io::{
    acquire_exclusive_file_lock, read_file_lock_holder, try_acquire_exclusive_file_lock,
};

pub fn read_lock_holder(path: &Path) -> Option<LockHolderInfo> {
    read_file_lock_holder(path)
}

/// Acquire an exclusive advisory lock on `path` using production defaults,
/// creating the file (and parent directories) if needed. `label` names the
/// operation for diagnostics (e.g. `"id allocation"`).
pub(crate) fn acquire_exclusive(path: &Path, label: &str) -> Result<FileLockGuard, OrbitError> {
    acquire_exclusive_with(path, label, LockOptions::default())
}

/// Single-shot exclusive acquisition: returns `Ok(None)` immediately when
/// another process holds the lock instead of waiting. For callers whose
/// correct response to contention is "someone else is already doing this
/// pass, exit" (e.g. the routine sweep) rather than queueing behind the
/// holder. The OS releases the lock on process death, so a crashed holder
/// never wedges future acquisitions.
pub(crate) fn try_acquire_exclusive(
    path: &Path,
    label: &str,
) -> Result<Option<FileLockGuard>, OrbitError> {
    try_acquire_exclusive_file_lock(path, label).map_err(OrbitError::from)
}

pub(crate) fn acquire_exclusive_with(
    path: &Path,
    label: &str,
    options: LockOptions,
) -> Result<FileLockGuard, OrbitError> {
    acquire_exclusive_file_lock(path, label, options).map_err(OrbitError::from)
}

#[cfg(test)]
mod tests;
