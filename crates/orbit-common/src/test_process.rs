//! Bounded retries for launching a freshly written executable on Linux.
//!
//! A test that copies an executable into a tempdir and immediately spawns it
//! races every other parallel test-process `fork()` in the suite: a sibling
//! spawn can briefly inherit a writable descriptor on *this* file across
//! `fork` before close-on-exec runs, and Linux rejects a concurrent exec of a
//! file that still looks open for writing with `ETXTBSY` (ORB-11340).
//! [`retry_executable_busy`] absorbs that bounded, load-dependent transient
//! for provider launches and test fixtures.
//!
//! Always available so provider runtime code and integration tests in sibling
//! crates share one implementation without changing `orbit-common`'s feature
//! set.

#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

/// How long [`retry_executable_busy`] keeps retrying before giving up.
#[cfg(target_os = "linux")]
const EXEC_BUSY_RETRY_WINDOW: Duration = Duration::from_secs(2);

/// Retry an executable launch while another process still has its writable
/// descriptor inherited across `fork`.
///
/// The descriptor is close-on-exec, but Linux can reject a concurrent exec
/// with `ETXTBSY` during that short pre-exec window. This is the same bounded
/// transient the production updater handles when it launches a newly written
/// binary; all other errors return immediately.
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
