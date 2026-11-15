//! Process-wide checkpoint at the start of each observed task-candidate key
//! selection. A test binary that serves the dashboard in a child process
//! installs one to hold selections open, proving that per-workspace scans
//! overlap; production never installs one, so the checkpoint is a no-op.

use std::sync::{Arc, Mutex, PoisonError};

use orbit_common::OrbitError;

type Checkpoint = Arc<dyn Fn() -> Result<(), OrbitError> + Send + Sync>;

static CHECKPOINT: Mutex<Option<Checkpoint>> = Mutex::new(None);

/// Run the installed checkpoint, if any, on the calling thread. A failing
/// checkpoint fails the selection it guards.
pub(crate) fn notify_candidate_selection() -> Result<(), OrbitError> {
    let checkpoint = CHECKPOINT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match checkpoint {
        Some(checkpoint) => checkpoint(),
        None => Ok(()),
    }
}

/// Run `checkpoint` at the start of every observed task-candidate key
/// selection in this process, from any thread. Last install wins.
#[cfg(feature = "test-support")]
pub fn install_candidate_selection_checkpoint(
    checkpoint: impl Fn() -> Result<(), OrbitError> + Send + Sync + 'static,
) {
    *CHECKPOINT.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(checkpoint));
}
