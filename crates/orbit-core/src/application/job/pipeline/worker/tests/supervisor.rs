//! Deterministic observer-channel failure after spawning a real child.

use std::process::Command;
use std::sync::mpsc;

use super::super::supervisor::{WorkerLaunchError, handoff_worker};

#[test]
fn a_closed_observer_channel_stops_and_reaps_the_spawned_child() {
    let (sender, receiver) = mpsc::sync_channel(1);
    drop(receiver);
    // Exec sleep directly: this fixture has no descendants to leak.
    let child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let error = handoff_worker(sender, child).unwrap_err();
    assert!(
        matches!(error, WorkerLaunchError::Uncertain(_)),
        "a spawned worker must never be reported as not started"
    );
    // Reaping matters: an unreaped killed child still has a process identity.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        -1,
        "observer handoff failure must stop and reap its worker"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
