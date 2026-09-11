//! Spawn helpers for tests that launch a freshly written executable.
//!
//! A test that copies an executable into a tempdir and immediately spawns it
//! races every other parallel test-process `fork()` in the suite: a sibling
//! spawn can briefly inherit a writable descriptor on *this* file across
//! `fork` before close-on-exec runs, and Linux rejects a concurrent exec of a
//! file that still looks open for writing with `ETXTBSY` (ORB-11340).
//! [`retry_executable_busy`] absorbs that bounded, load-dependent transient so
//! the fixture fails only on a real spawn error.
//!
//! Exposed behind the `test-util` feature so integration tests in sibling
//! crates, which cannot see this crate's `#[cfg(test)]` items, share one
//! implementation rather than each re-deriving the retry.

use std::io;
use std::time::{Duration, Instant};

/// How long [`retry_executable_busy`] keeps retrying before giving up.
#[cfg(target_os = "linux")]
const EXEC_BUSY_RETRY_WINDOW: Duration = Duration::from_secs(2);

/// Retry a freshly copied test launcher while another parallel test's child
/// still has its writable descriptor inherited across `fork`.
///
/// The descriptor is close-on-exec, but Linux can reject a concurrent exec
/// with `ETXTBSY` during that short pre-exec window. This is the same bounded
/// transient the production updater handles when it launches a newly written
/// binary; all other errors remain immediate test failures.
#[cfg(target_os = "linux")]
pub fn retry_executable_busy<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let deadline = Instant::now() + EXEC_BUSY_RETRY_WINDOW;
    loop {
        match operation() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}
