use std::time::Duration;

use tempfile::TempDir;

use super::super::{LockOptions, acquire_exclusive, acquire_exclusive_with, read_lock_holder};

/// Exact libtest name of the ignored helper below, re-exec'd as a child process
/// by [`sigkilled_holder_releases_lock`]. Keep in sync with the module path.
#[cfg(unix)]
const CRASH_CHILD_TEST: &str = "fs::lock::tests::file_lock::crash_holder_child";

fn short_options(timeout_ms: u64) -> LockOptions {
    LockOptions {
        timeout: Duration::from_millis(timeout_ms),
        // Push the warn threshold out of the way unless a test wants it.
        warn_after: Duration::from_secs(3600),
    }
}

/// Crash semantics: the OS releases advisory (`flock`) locks when a holder
/// process dies, so a hung/crashed holder never wedges the workspace forever.
/// A child process takes the lock, we SIGKILL it, and a subsequent acquisition
/// by the parent must succeed. Documents the assumption the timeout targets the
/// *hung* (not crashed) holder.
#[cfg(unix)]
#[test]
fn sigkilled_holder_releases_lock() {
    use std::os::unix::fs::MetadataExt;

    let dir = TempDir::new().expect("tempdir");
    let lock_path = dir.path().join("crash.lock");
    let ready_path = dir.path().join("ready");

    orbit_common::test_env::assert_child_test_exists(CRASH_CHILD_TEST);
    let exe = std::env::current_exe().expect("current test exe");
    let mut child = std::process::Command::new(exe)
        .args(["--exact", CRASH_CHILD_TEST, "--ignored"])
        .env("ORBIT_FILE_LOCK_CRASH_LOCK", &lock_path)
        .env("ORBIT_FILE_LOCK_CRASH_READY", &ready_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn lock-holder child");

    // Wait for the child to signal it holds the lock.
    let start = std::time::Instant::now();
    while !ready_path.exists() {
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child never acquired the lock");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let child_pid = child.id();
    let inode = std::fs::metadata(&lock_path).expect("lock metadata").ino();

    // While the child holds it, the parent cannot acquire within a short budget.
    let held = acquire_exclusive_with(&lock_path, "parent-probe", short_options(200));
    assert!(held.is_err(), "lock should be held by the live child");

    // Crash the holder: Child::kill() sends SIGKILL on Unix.
    child.kill().expect("SIGKILL child");
    child.wait().expect("reap child");
    let stale = read_lock_holder(&lock_path).expect("crash leaves diagnostic metadata");
    assert_eq!(stale.pid, child_pid);
    assert_eq!(stale.label, "crash-holder");

    // The advisory lock is released on process death: acquisition now succeeds.
    let guard = acquire_exclusive_with(
        &lock_path,
        "parent-after-crash",
        LockOptions {
            timeout: Duration::from_secs(10),
            warn_after: Duration::from_secs(3600),
        },
    );
    let guard = guard.expect("lock released after holder was SIGKILLed");
    assert_eq!(
        std::fs::metadata(&lock_path).expect("lock retained").ino(),
        inode,
        "recovery must acquire the existing lock file"
    );
    drop(guard);
    assert!(
        read_lock_holder(&lock_path).is_none(),
        "clean reacquisition clears the stale crash metadata"
    );
}

/// Re-exec'd as a child process by [`sigkilled_holder_releases_lock`]. Ignored
/// so it never runs on its own; when invoked with the crash env vars it takes
/// the lock, signals readiness via a sentinel file, and blocks until killed.
#[cfg(unix)]
#[test]
#[ignore = "helper process for sigkilled_holder_releases_lock; re-exec'd, not run directly"]
fn crash_holder_child() {
    let (Ok(lock_path), Ok(ready_path)) = (
        std::env::var("ORBIT_FILE_LOCK_CRASH_LOCK"),
        std::env::var("ORBIT_FILE_LOCK_CRASH_READY"),
    ) else {
        return;
    };

    let _guard =
        acquire_exclusive(std::path::Path::new(&lock_path), "crash-holder").expect("child acquire");
    std::fs::write(&ready_path, b"ready").expect("write ready sentinel");
    // Hold the lock until the parent SIGKILLs us.
    std::thread::sleep(Duration::from_secs(60));
}
