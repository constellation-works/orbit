//! Scoped worker threads for the doctor's parallel scans.
//!
//! `std::thread::Scope::spawn` panics when the OS refuses a thread, and the
//! CPU-count estimate that sizes each pool says nothing about free thread
//! slots. The scans therefore start workers through [`spawn_scan_worker`],
//! which reports the refusal, and run whatever work no worker took on the
//! calling thread.

use std::io;
use std::thread::{Scope, ScopedJoinHandle};

/// Start one scoped worker, returning the OS refusal instead of panicking.
pub(super) fn spawn_scan_worker<'scope, 'env, T: Send + 'scope>(
    scope: &'scope Scope<'scope, 'env>,
    work: impl FnOnce() -> T + Send + 'scope,
) -> io::Result<ScopedJoinHandle<'scope, T>> {
    #[cfg(feature = "test-support")]
    if seam::refuse() {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    std::thread::Builder::new()
        .name("orbit-doctor-scan".into())
        .spawn_scoped(scope, work)
}

/// Pool size to use while the test-support `DoctorWorkerSpawnRefusal` guard is
/// held: `work_items` capped at `max`. The pool takes its threaded path, and
/// so attempts a spawn through [`spawn_scan_worker`], only with two or more
/// work items. With zero or one item it runs on the calling thread, attempts
/// no spawn, and the guard refuses nothing. The seam ignores the host's
/// parallelism. `None` outside the seam, where the host's parallelism sizes
/// the pool.
pub(super) fn forced_pool_size(max: usize, work_items: usize) -> Option<usize> {
    #[cfg(feature = "test-support")]
    if seam::installed() {
        return Some(work_items.min(max));
    }
    let _ = (max, work_items);
    None
}

/// Per-thread seam that makes every doctor scan worker spawn attempt fail the
/// way an exhausted thread limit does, without placing load on the host. A scan
/// attempts a spawn only with two or more work items: subdirectories of one
/// directory, or doctor probes. With zero or one item it runs on the calling
/// thread, and `refused` stays at zero.
#[cfg(feature = "test-support")]
pub struct DoctorWorkerSpawnRefusal;

#[cfg(feature = "test-support")]
impl DoctorWorkerSpawnRefusal {
    /// Refuse every worker spawn on this thread until the guard drops.
    pub fn install() -> Self {
        seam::REFUSED.with(|slot| slot.set(Some(0)));
        Self
    }

    /// Worker spawns refused since [`Self::install`].
    pub fn refused(&self) -> usize {
        seam::REFUSED.with(|slot| slot.get().unwrap_or(0))
    }
}

#[cfg(feature = "test-support")]
impl Drop for DoctorWorkerSpawnRefusal {
    fn drop(&mut self) {
        seam::REFUSED.with(|slot| slot.set(None));
    }
}

#[cfg(feature = "test-support")]
mod seam {
    use std::cell::Cell;

    thread_local! {
        pub(super) static REFUSED: Cell<Option<usize>> = const { Cell::new(None) };
    }

    pub(super) fn installed() -> bool {
        REFUSED.with(|slot| slot.get().is_some())
    }

    pub(super) fn refuse() -> bool {
        REFUSED.with(|slot| {
            slot.get().is_some_and(|count| {
                slot.set(Some(count + 1));
                true
            })
        })
    }
}
