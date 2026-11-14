//! Unit admission: deterministic lock-holder interleaving during stale-lock cleanup.
#![cfg(unix)]
use std::fs;
use std::path::Path;

use chrono::Utc;
use fs2::FileExt;

fn reaped_child_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

/// Reaps the child on drop without touching it earlier: any `try_wait`/`wait`
/// before the stale assertions would reap the zombie the test needs.
struct ReapingChild(std::process::Child);

impl Drop for ReapingChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn write_holder_lock(path: &Path, pid: u32, label: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create lock dir");
    }
    fs::write(
        path,
        serde_json::to_string(&serde_json::json!({
            "pid": pid,
            "acquired_at": Utc::now().to_rfc3339(),
            "label": label,
        }))
        .expect("serialize holder"),
    )
    .expect("write lock file");
}

// Deterministic interleaving admission: the CLI boundary cannot force an
// opener to queue while doctor owns the lock. Guards the orphaned-inode race.
#[test]
fn stale_lock_cleanup_preserves_mutual_exclusion_for_queued_openers() {
    use std::os::unix::fs::MetadataExt;

    use crate::doctor::task::remove_stale_lock_file_after_acquire;

    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &stale_lock_cleanup_preserves_mutual_exclusion_for_queued_openers,
    )) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let lock_path = temp.path().join("state/layout.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "crashed layout upgrade");
    let original = fs::metadata(&lock_path).expect("original lock metadata");
    let mut queued = None;

    assert!(
        remove_stale_lock_file_after_acquire(&lock_path, || {
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock_path)
                .expect("open queued descriptor while cleanup holds the lock");
            assert_eq!(
                file.try_lock_exclusive()
                    .expect_err("queued opener must be refused while cleanup holds the lock")
                    .kind(),
                std::io::ErrorKind::WouldBlock
            );
            queued = Some(file);
        })
        .expect("clean stale holder")
    );

    let queued = queued.expect("queued descriptor");
    queued
        .try_lock_exclusive()
        .expect("queued descriptor acquires after cleanup releases");
    let fresh = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock after cleanup");
    assert_eq!(
        fresh
            .try_lock_exclusive()
            .expect_err("ORB-14069: a new opener must not acquire alongside the queued descriptor")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let preserved = fresh.metadata().expect("preserved lock metadata");
    assert_eq!(
        (preserved.dev(), preserved.ino()),
        (original.dev(), original.ino())
    );
    assert_eq!(preserved.len(), 0, "cleanup clears only the holder record");
    assert!(orbit_store::read_lock_holder(&lock_path).is_none());
    drop(queued);
    fresh
        .try_lock_exclusive()
        .expect("new opener acquires after the queued descriptor releases");
}

// A holder that exited but was never reaped still accepts signal 0, so the
// old `kill(pid, 0)` probe counted it as live and neither reported nor
// cleared its lock. The zombie-aware owner reports it dead.
#[test]
fn stale_lock_cleanup_clears_zombie_holder() {
    use std::time::{Duration, Instant};

    use crate::doctor::task::remove_stale_lock_file;

    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &stale_lock_cleanup_clears_zombie_holder,
    )) {
        return;
    }

    // Spawn a child that exits at once and never reap it until the end, so
    // the pid spends the test as an unreaped zombie. Nothing may call
    // `try_wait`/`wait` on it before the stale assertions.
    let mut child = ReapingChild(
        std::process::Command::new("true")
            .spawn()
            .expect("spawn child"),
    );
    let pid = child.0.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    while crate::doctor::process_is_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    // The old probe still sees the zombie as alive: signal 0 is delivered.
    let raw_probe = unsafe { libc::kill(pid as libc::pid_t, 0) };
    assert!(
        raw_probe == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM),
        "zombie holder must still accept signal 0 (pid {pid})"
    );
    assert!(
        !crate::doctor::process_is_alive(pid),
        "zombie holder pid {pid} must not count as live"
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let lock_path = temp.path().join("state/layout.lock");
    write_holder_lock(&lock_path, pid, "crashed layout upgrade");
    assert!(
        remove_stale_lock_file(&lock_path).expect("clean zombie holder"),
        "a lock whose holder is a zombie must be reported stale and cleared"
    );
    assert!(orbit_store::read_lock_holder(&lock_path).is_none());
    child.0.wait().expect("reap zombie");
}
