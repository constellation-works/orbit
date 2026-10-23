//! Workspace confinement, symlink-safe removal and advisory file locks.

use std::io;

use orbit_common::fs::io::with_exclusive_file_lock;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn remove_path_if_exists_unlinks_a_directory_symlink_without_removing_its_target() {
    use orbit_common::fs::io::remove_path_if_exists;

    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).expect("target directory");
    let target_file = target.join("keep");
    std::fs::write(&target_file, b"preserved").expect("target file");
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");

    remove_path_if_exists(&link).expect("remove directory symlink");

    let error = std::fs::symlink_metadata(&link).expect_err("link must be removed");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(
        std::fs::read(target_file).expect("target preserved"),
        b"preserved"
    );
}

#[cfg(unix)]
#[test]
fn symlink_that_escapes_is_refused() {
    use orbit_common::OrbitError;
    use orbit_common::fs::cwd::confine_workspace_cwd;

    let root = TempDir::new().expect("tempdir");
    let checkout = root.path().join("repo");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let checkout = checkout.canonicalize().expect("canonicalize checkout");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let link = checkout.join("escape");
    std::os::unix::fs::symlink(&outside, &link).expect("symlink that escapes");

    let error = confine_workspace_cwd(
        "working_directory",
        &link.display().to_string(),
        "ws_fixture",
        &checkout,
        &[],
    )
    .expect_err("a symlink resolving outside the checkout is refused");
    let message = match error {
        OrbitError::InvalidInput(message) => message,
        other => panic!("expected invalid input, got {other:?}"),
    };
    assert!(message.contains("outside workspace"), "{message}");
}

/// Lock files are opened through the private-file open with create, read and
/// write, so a lock file left permissive by an older binary or another tool is
/// restricted to its owner when it is next opened.
#[cfg(unix)]
#[test]
fn private_open_repairs_a_permissive_existing_file() {
    use std::os::unix::fs::PermissionsExt;

    use orbit_common::fs::io::try_acquire_exclusive_file_lock;

    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join("shared.lock");
    std::fs::write(&path, b"").expect("create lock file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("make the file permissive");

    let _guard = try_acquire_exclusive_file_lock(&path, "shared")
        .expect("open private")
        .expect("lock is free");

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o600, "a permissive file is restricted to its owner");
}

/// ORB-10988: nesting the same lock path on one thread must re-enter, not
/// deadlock. `flock(2)` belongs to the open file description, so the inner
/// call's fresh descriptor would otherwise block on the outer call's lock
/// forever. The runtime relies on this to hold a task lock across a
/// read-modify-write whose inner store writes lock the same file.
#[test]
fn exclusive_lock_is_reentrant_within_a_thread() {
    let temp = TempDir::new().expect("tempdir");
    let target = temp.path().join("bundle").join("task.yaml");

    let depth = with_exclusive_file_lock::<usize, io::Error, _>(&target, "outer", || {
        with_exclusive_file_lock::<usize, io::Error, _>(&target, "inner", || {
            with_exclusive_file_lock::<usize, io::Error, _>(&target, "innermost", || Ok(3))
        })
    })
    .expect("nested locks must re-enter");

    assert_eq!(depth, 3);
}

/// The outer call is the one that creates the parent directory, so the key
/// re-entrancy is tracked under must not depend on whether the parent existed
/// when the call started. Reaching the target through a symlink is what makes
/// the resolved and literal paths differ, and that difference used to leave a
/// nested call unable to see the lock it already held — it opened a second
/// descriptor to the same file and blocked forever.
///
/// The work runs on its own thread so a regression fails on the timeout rather
/// than hanging the suite. macOS reproduces this without the explicit symlink,
/// because its temp directories already sit under one; the symlink here is what
/// makes the case reproduce on Linux too.
#[cfg(unix)]
#[test]
fn exclusive_lock_re_enters_when_the_outer_call_creates_the_parent_under_a_symlink() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = TempDir::new().expect("tempdir");
    let real_root = temp.path().join("real");
    std::fs::create_dir_all(&real_root).expect("real root");
    let link_root = temp.path().join("link");
    std::os::unix::fs::symlink(&real_root, &link_root).expect("symlink to the real root");

    // `bundle` deliberately does not exist yet.
    let target = link_root.join("bundle").join("task.yaml");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let depth = with_exclusive_file_lock::<usize, io::Error, _>(&target, "outer", || {
            with_exclusive_file_lock::<usize, io::Error, _>(&target, "inner", || Ok(2))
        });
        let _ = tx.send(depth);
    });

    let depth = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("nested lock deadlocked instead of re-entering")
        .expect("nested locks must re-enter");
    assert_eq!(depth, 2);
}

/// Criterion: a descriptor a forked child inherited is not a holder.
///
/// `flock` locks belong to the open file description, and `fork` copies the
/// descriptor table, so a child forked while the lock was held keeps holding it
/// after the parent drops its guard — until `execve` closes the `O_CLOEXEC`
/// descriptor. Before [ORB-12532] the next acquisition read that as contention,
/// which is how the routine sweep intermittently skipped a whole pass and
/// returned an empty `SweepOutcome` under a loaded parallel test suite.
///
/// The child here never execs, so it holds the inherited descriptor for the
/// whole window deterministically rather than only under load.
#[cfg(unix)]
#[test]
fn a_descriptor_inherited_by_a_forked_child_is_not_read_as_contention() {
    use orbit_common::fs::io::{read_file_lock_holder, try_acquire_exclusive_file_lock};

    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("state/routine-sweep.lock");

    let held = try_acquire_exclusive_file_lock(&path, "routine sweep")
        .expect("first acquisition")
        .expect("lock is free");

    // SAFETY: the child only calls async-signal-safe functions before `_exit`.
    let child = unsafe { libc::fork() };
    assert!(
        child >= 0,
        "fork failed: {}",
        std::io::Error::last_os_error()
    );
    if child == 0 {
        // The inherited descriptor keeps the lock held for this long.
        unsafe {
            libc::usleep(300_000);
            libc::_exit(0);
        }
    }

    // The pass finished: nothing claims the lock, only the inherited copy
    // still refers to the locked open file description.
    drop(held);
    assert!(
        read_file_lock_holder(&path).is_none(),
        "dropping the guard clears the holder record"
    );

    let regained = try_acquire_exclusive_file_lock(&path, "routine sweep")
        .expect("re-acquisition")
        .expect("an inherited descriptor no holder claims is not contention");
    drop(regained);

    let mut status = 0;
    // SAFETY: `child` is this process's direct child; `status` outlives the call.
    assert!(
        unsafe { libc::waitpid(child, &mut status, 0) } == child,
        "reap the child"
    );
}
