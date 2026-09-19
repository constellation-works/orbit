//! Single-shot advisory acquisition under descriptor inheritance.

use std::time::Duration;

use crate::fs::file_lock::{read_file_lock_holder, try_acquire_exclusive_file_lock};

#[test]
fn a_recorded_holder_refuses_a_second_acquisition_immediately() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("state/routine-sweep.lock");

    let _held = try_acquire_exclusive_file_lock(&path, "routine sweep")
        .expect("first acquisition")
        .expect("lock is free");
    assert_eq!(
        read_file_lock_holder(&path).expect("holder recorded").label,
        "routine sweep"
    );

    let started = std::time::Instant::now();
    let second = try_acquire_exclusive_file_lock(&path, "routine sweep").expect("second attempt");
    assert!(
        second.is_none(),
        "a live holder must refuse the second pass"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a claimed refusal must not queue behind the holder: waited {:?}",
        started.elapsed()
    );
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
