//! Ownership and safe teardown of a spawned process-group leader.

use std::io;
use std::process::Child;
#[cfg(unix)]
use std::thread;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

#[cfg(unix)]
use super::identity::{KernelLiveness, probe_process_group_liveness};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
#[cfg(unix)]
const GROUP_DRAIN_BUDGET: Duration = Duration::from_secs(1);

/// Own a child from spawn until its leader and remaining group are cleaned up.
///
/// Unless [`Self::mark_reaped`] was called, dropping this handle kills the
/// group, reaps the leader and waits at most one second for the group to drain.
/// On non-Unix platforms only the direct child is killed.
#[derive(Debug)]
pub struct OwnedChild {
    child: Child,
    reaped: bool,
}

impl OwnedChild {
    /// Adopt a spawned child. On Unix the caller must have spawned it as its
    /// own process-group leader, for example with `Command::process_group(0)`.
    pub fn from_spawned(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    /// The leader's process ID.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Borrow the child for pipe setup or waiting. After reaping it, finish
    /// any descendant cleanup and call [`Self::mark_reaped`].
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Signal the owned group, unless cleanup was marked complete.
    ///
    /// Never signals the caller's group or a PID now belonging to another
    /// group. On non-Unix platforms this does nothing.
    ///
    /// # Errors
    /// Returns an OS signal error other than an already-absent group.
    pub fn signal_group(&self, signal: i32) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        Self::signal_spawned_group(self.pid(), signal)
    }

    /// Send TERM, wait for the entire group for `grace`, then kill and reap.
    ///
    /// # Errors
    /// Returns a child wait error; ownership remains armed on failure.
    pub fn terminate(&mut self, grace: Duration) -> io::Result<()> {
        self.terminate_with_signal(15, grace, POLL_INTERVAL)
    }

    /// Like [`Self::terminate`], with the caller's termination signal and poll
    /// interval. This preserves supervisors that forward INT or TERM.
    ///
    /// # Errors
    /// Returns a child wait error; ownership remains armed on failure.
    pub fn terminate_with_signal(
        &mut self,
        signal: i32,
        grace: Duration,
        poll_interval: Duration,
    ) -> io::Result<()> {
        if !self.reaped {
            Self::terminate_spawned(&mut self.child, signal, grace, poll_interval)?;
            self.mark_reaped();
        }
        Ok(())
    }

    /// Kill the group, reap the leader, and wait a bounded time for the group.
    /// Cleanup errors are best effort, as they are on drop.
    pub fn kill_and_reap(&mut self) {
        if !self.reaped {
            Self::kill_and_reap_spawned(&mut self.child);
            self.mark_reaped();
        }
    }

    /// Disarm cleanup after reaping the leader and cleaning up descendants.
    /// No subsequent operation on this handle signals its former PID.
    pub fn mark_reaped(&mut self) {
        self.reaped = true;
    }

    /// Signal a group known to have been led by a spawned child. This adapter
    /// also supports cleanup of descendants after their leader was reaped.
    /// Ownership-aware callers should prefer [`Self::signal_group`].
    ///
    /// # Errors
    /// Returns an OS signal error other than an already-absent group.
    pub fn signal_spawned_group(leader: u32, signal: i32) -> io::Result<()> {
        #[cfg(unix)]
        {
            let Some(leader) = libc::pid_t::try_from(leader).ok().filter(|pid| *pid > 1) else {
                return Ok(());
            };
            // SAFETY: these calls only query or signal process groups; they do
            // not dereference caller memory. Never target our own group or a
            // reused PID that no longer leads the spawned group.
            let own = unsafe { libc::getpgrp() };
            if leader == own {
                return Ok(());
            }
            let current = unsafe { libc::getpgid(leader) };
            if current > 1 && current != leader {
                return Ok(());
            }
            if unsafe { libc::killpg(leader, signal) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = (leader, signal);
        Ok(())
    }

    /// Transitional adapter for callers still holding a bare spawned child.
    /// Uses the same safe group signals and bounded drain as owned cleanup.
    pub fn kill_and_reap_spawned(child: &mut Child) {
        let pid = child.id();
        let _ = Self::signal_spawned_group(pid, 9);
        let _ = child.kill();
        let _ = child.wait();
        #[cfg(unix)]
        drain_group(pid);
    }

    /// Transitional adapter for a bare spawned child, including descendants
    /// whose leader has already exited. Uses the owned handle's signal checks.
    ///
    /// # Errors
    /// Returns an error if polling the child fails.
    pub fn terminate_spawned(
        child: &mut Child,
        signal: i32,
        grace: Duration,
        poll_interval: Duration,
    ) -> io::Result<()> {
        #[cfg(unix)]
        {
            let pid = child.id();
            let _ = Self::signal_spawned_group(pid, signal);
            let deadline = Instant::now() + grace;
            loop {
                child.try_wait()?;
                if group_exited(pid) {
                    let _ = child.wait();
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(poll_interval);
            }
        }
        #[cfg(not(unix))]
        let _ = (signal, grace, poll_interval);
        Self::kill_and_reap_spawned(child);
        Ok(())
    }
}

impl From<Child> for OwnedChild {
    fn from(child: Child) -> Self {
        Self::from_spawned(child)
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

#[cfg(unix)]
fn group_exited(pid: u32) -> bool {
    let pgid = libc::pid_t::try_from(pid).unwrap_or(0);
    probe_process_group_liveness(pgid) == KernelLiveness::Exited
}

#[cfg(unix)]
fn drain_group(pid: u32) {
    let deadline = Instant::now() + GROUP_DRAIN_BUDGET;
    while !group_exited(pid) {
        let _ = OwnedChild::signal_spawned_group(pid, 9);
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
}
