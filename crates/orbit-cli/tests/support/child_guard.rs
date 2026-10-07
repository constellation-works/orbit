//! Cleanup for long-lived CLI fixture processes, including assertion unwinds.

use std::ops::{Deref, DerefMut};
use std::process::Child;
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Own a child immediately after spawning, before any readiness assertions.
///
/// The original PID remains valid across an executable handover. Cleanup
/// checks for an already-reaped child before signaling, so an explicit wait
/// never causes a later drop to signal a reused PID.
pub(crate) struct ChildGuard(Child);

impl ChildGuard {
    pub(crate) fn new(child: Child) -> Self {
        Self(child)
    }
}

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        #[cfg(unix)]
        {
            // SAFETY: this PID belongs to our unreaped child, even after exec.
            // Cleanup must not panic while unwinding an assertion failure.
            unsafe { libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match self.0.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => break,
                }
            }
        }
        // SIGKILL on Unix; also supplies cleanup on platforms without SIGTERM.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
