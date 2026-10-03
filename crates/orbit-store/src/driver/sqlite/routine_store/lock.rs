//! Host-global sweep advisory lock.

use std::path::Path;

use orbit_common::OrbitError;

use crate::fs::lock::{self, FileLockGuard};

/// Lock file name under the global state dir guarding one sweep pass per host.
const SWEEP_LOCK_FILE: &str = "routine-sweep.lock";

/// RAII guard over the host-global sweep advisory lock
/// (`docs/design-patterns/raii_guard.md`): dropping it releases the lock.
#[derive(Debug)]
#[must_use = "the sweep lock is released as soon as the guard is dropped"]
pub struct RoutineSweepLock {
    _guard: FileLockGuard,
}

/// Try to take this host's sweep lock without queueing behind a live holder.
/// `Ok(None)` means another sweep pass is in flight and the caller should exit
/// cleanly — overlapping invocations from a slow prior pass must not
/// double-fire. A refusal no holder claims is not such a pass; see
/// `orbit_common::fs::file_lock::try_acquire_exclusive_file_lock`.
pub fn try_acquire_routine_sweep_lock(
    global_state_dir: &Path,
) -> Result<Option<RoutineSweepLock>, OrbitError> {
    let path = global_state_dir.join(SWEEP_LOCK_FILE);
    Ok(lock::try_acquire_exclusive(&path, "routine sweep")?
        .map(|guard| RoutineSweepLock { _guard: guard }))
}
