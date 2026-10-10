//! Reaps a re-executed fixture child's process group when its test parent dies.
//!
//! [`run_child_test`](super::run_child_test) isolates each child in its own
//! process group so the parent can kill the whole subtree, but that group is
//! outside the parent's own: when nextest interrupts or times out the parent,
//! nothing signals the child group and it keeps running (ORB-15240).
//! `PR_SET_PDEATHSIG` would reach only the direct child, and not on macOS, so
//! a small watchdog process owns the cleanup instead. It sits in its own
//! group (nextest's kill of the parent's group cannot reach it), reads a pipe
//! whose only write end the parent holds, and on end-of-file, which the
//! kernel delivers however the parent died, SIGKILLs the child's group.

/// A watchdog tied to the lifetime of the process that created it.
#[cfg(unix)]
pub(super) struct OrphanWatchdog {
    process: std::process::Child,
}

#[cfg(unix)]
impl OrphanWatchdog {
    /// Watch the process group led by `leader`, which was spawned with
    /// [`isolate_process_group`](crate::process::bounded::isolate_process_group).
    ///
    /// Returns `None` when the watchdog cannot start; the caller then keeps
    /// the pre-existing behaviour of cleaning up only on its own wait path.
    pub(super) fn arm(leader: u32) -> Option<Self> {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        // Nothing but the pipe's end-of-file wakes the loop: the parent never
        // writes. The group id arrives as `$1`, never spliced into the script.
        const SCRIPT: &str = r#"while read -r _; do :; done; kill -KILL "-$1""#;
        let mut command = Command::new("sh");
        command
            .args(["-c", SCRIPT, "orbit-fixture-watchdog"])
            .arg(leader.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        command.spawn().ok().map(|process| Self { process })
    }

    /// Stand down after the parent has reaped the group itself, so a recycled
    /// group id is never signalled.
    pub(super) fn disarm(mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Fixture children are only isolated into a process group on Unix.
#[cfg(not(unix))]
pub(super) struct OrphanWatchdog;

#[cfg(not(unix))]
impl OrphanWatchdog {
    pub(super) fn arm(_leader: u32) -> Option<Self> {
        None
    }

    pub(super) fn disarm(self) {}
}
