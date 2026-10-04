use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

pub(super) const TERMINATION_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[cfg(unix)]
pub(crate) fn terminate_process_group(
    child: &mut Child,
    signal: i32,
    poll_interval: Duration,
) -> Result<(), OrbitError> {
    let pid = child.id();
    send_signal_to_process_group(pid, signal);
    if wait_for_process_group_exit(pid, Some(child), TERMINATION_GRACE_PERIOD, poll_interval)? {
        return Ok(());
    }
    kill_process_group(pid);
    child.kill().ok();
    child.wait().ok();
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn terminate_process_group(
    child: &mut Child,
    _signal: i32,
    _poll_interval: Duration,
) -> Result<(), OrbitError> {
    child.kill().ok();
    child.wait().ok();
    Ok(())
}

#[cfg(unix)]
pub(super) fn terminate_orphaned_process_group(pid: u32, signal: i32, poll_interval: Duration) {
    send_signal_to_process_group(pid, signal);
    if wait_for_process_group_exit(pid, None, TERMINATION_GRACE_PERIOD, poll_interval)
        .unwrap_or(false)
    {
        return;
    }
    kill_process_group(pid);
}

#[cfg(unix)]
pub(crate) fn termination_signal() -> i32 {
    libc::SIGTERM
}

#[cfg(not(unix))]
pub(crate) fn termination_signal() -> i32 {
    15
}

/// Send SIGKILL to every process in the child's process group.
///
/// Because we spawn children with `process_group(0)` the child's PGID equals
/// its PID, so `killpg` covers the child itself plus any subprocesses it
/// started that did not create their own group.
#[cfg(unix)]
pub(super) fn kill_process_group(pid: u32) {
    send_signal_to_process_group(pid, libc::SIGKILL);
}

#[cfg(not(unix))]
pub(super) fn kill_process_group(_pid: u32) {}

#[cfg(unix)]
fn send_signal_to_process_group(pid: u32, signal: i32) {
    // Safety: `killpg` is async-signal-safe and only sends a signal; no
    // undefined behaviour from the call itself.
    unsafe {
        libc::killpg(pid as libc::pid_t, signal);
    }
}

#[cfg(unix)]
fn wait_for_process_group_exit(
    pid: u32,
    mut child: Option<&mut Child>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<bool, OrbitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(child) = child.as_deref_mut() {
            child
                .try_wait()
                .map_err(|e| OrbitError::Execution(format!("failed waiting for process: {e}")))?;
        }
        if !process_group_is_alive(pid) {
            if let Some(child) = child.as_deref_mut() {
                child.wait().ok();
            }
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(poll_interval);
    }
}

#[cfg(unix)]
fn process_group_is_alive(pid: u32) -> bool {
    // `killpg(..., 0)` checks whether any process still belongs to the group.
    let rc = unsafe { libc::killpg(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    )
}
