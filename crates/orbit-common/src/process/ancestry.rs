//! Process ancestry keyed by pid plus start time, so a recorded child can be
//! recognised after it unsets its environment.
//!
//! A pid alone is reused. Pairing it with the kernel start time is what lets
//! a later `orbit tool run` (or MCP server) decide it is still the process the
//! host launched as a plugin backend, or a descendant of one.

/// A live process as the kernel names it: pid plus start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessStartKey {
    pub pid: u32,
    /// Kernel start time. Linux: `/proc/<pid>/stat` field 22 (clock ticks
    /// since boot). macOS: `pbi_start_tvsec` packed with microseconds.
    pub starttime: u64,
}

/// The start key for `pid`, when the kernel still describes that process.
pub fn process_start_key(pid: u32) -> Option<ProcessStartKey> {
    inspect_process(pid).map(|(starttime, _ppid)| ProcessStartKey { pid, starttime })
}

/// Walk from this process toward pid 1, including self. Stops on a kernel
/// read failure, a self-parent, or a bounded depth so a pid cycle cannot
/// loop the caller.
///
/// This reads `/proc/<pid>` (Linux) or libproc (macOS). A Landlock-confined
/// plugin child cannot do that for another process; [`current_process_group`]
/// and [`current_parent_pid`] are the syscalls that still work there.
pub fn ancestor_start_keys() -> Vec<ProcessStartKey> {
    let mut pid = std::process::id();
    let mut out = Vec::new();
    for _ in 0..64 {
        let Some((starttime, ppid)) = inspect_process(pid) else {
            break;
        };
        out.push(ProcessStartKey { pid, starttime });
        if ppid == 0 || ppid == pid {
            break;
        }
        pid = ppid;
    }
    out
}

/// This process's process-group id (`getpgrp`). Orbit spawns plugin backends
/// with `process_group(0)`, so the child's PGID equals its PID and every
/// descendant — including `orbit tool run` started from a `$()` subshell —
/// inherits that group. The syscall does not consult `/proc`.
#[cfg(unix)]
pub fn current_process_group() -> Option<u32> {
    // Safety: `getpgrp` is a pure query of the calling process.
    let pgid = unsafe { libc::getpgrp() };
    u32::try_from(pgid).ok().filter(|pgid| *pgid > 1)
}

#[cfg(not(unix))]
pub fn current_process_group() -> Option<u32> {
    None
}

/// This process's parent pid (`getppid`). Same Landlock constraint as
/// [`current_process_group`]: a syscall, not a `/proc` read.
#[cfg(unix)]
pub fn current_parent_pid() -> Option<u32> {
    // Safety: `getppid` is a pure query of the calling process.
    let ppid = unsafe { libc::getppid() };
    u32::try_from(ppid).ok().filter(|ppid| *ppid > 0)
}

#[cfg(not(unix))]
pub fn current_parent_pid() -> Option<u32> {
    None
}

fn inspect_process(pid: u32) -> Option<(u64, u32)> {
    #[cfg(target_os = "linux")]
    {
        linux_start_and_parent(pid)
    }
    #[cfg(target_os = "macos")]
    {
        darwin_start_and_parent(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_start_and_parent(pid: u32) -> Option<(u64, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, tail) = stat.rsplit_once(')')?;
    let mut fields = tail.split_whitespace();
    let _state = fields.next()?;
    let ppid: u32 = fields.next()?.parse().ok()?;
    // After `ppid`: pgrp session tty_nr tpgid flags minflt cminflt majflt
    // cmajflt utime stime cutime cstime priority nice num_threads itrealvalue
    // starttime. That is 17 further fields to skip, then starttime.
    for _ in 0..17 {
        fields.next()?;
    }
    let starttime: u64 = fields.next()?.parse().ok()?;
    Some((starttime, ppid))
}

#[cfg(target_os = "macos")]
fn darwin_start_and_parent(pid: u32) -> Option<(u64, u32)> {
    use std::mem::{MaybeUninit, size_of};

    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let expected = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // Safety: `info` points to `expected` writable bytes and is initialized
    // only when libproc reports that it filled the complete structure.
    let written = unsafe {
        libc::proc_pidinfo(
            pid as libc::pid_t,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if written != expected {
        return None;
    }
    // Safety: the full structure was written above.
    let info = unsafe { info.assume_init() };
    let starttime = (u64::from(info.pbi_start_tvsec) << 20)
        | u64::from(info.pbi_start_tvusec).min((1 << 20) - 1);
    Some((starttime, info.pbi_ppid))
}
