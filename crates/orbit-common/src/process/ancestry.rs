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

/// The start key and parent pid of `pid`, read together so a caller walking
/// ancestry can check each parent against the child it came from.
pub fn process_start_and_parent(pid: u32) -> Option<(ProcessStartKey, u32)> {
    inspect_process(pid).map(|(starttime, ppid)| (ProcessStartKey { pid, starttime }, ppid))
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
    let starttime = (info.pbi_start_tvsec << 20) | info.pbi_start_tvusec.min((1 << 20) - 1);
    Some((starttime, info.pbi_ppid))
}

/// One live process in a worker's tree, named so a reused pid is not the
/// process sampled last time.
///
/// CPU time is this process's own user and system time. It does not include
/// children it has waited for: adding those on top of the children's own
/// samples would count the same work twice when a child exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessTreeProcess {
    pub key: ProcessStartKey,
    /// Own user plus system CPU time, in nanoseconds.
    pub cpu_time_ns: u64,
    /// Non-reclaimable physical footprint (`ri_phys_footprint`), in bytes.
    /// Resident size counts file-backed and shared pages, which overstates
    /// the tree the way `memory.current` does on Linux.
    pub phys_footprint: u64,
}

/// The live process tree rooted at `root`, including `root`.
///
/// `None` when the tree cannot be read: the root itself is unreadable, a
/// descendant listing is denied or truncated, or a process that is still
/// listed cannot be measured. A descendant that exits, is replaced, or is
/// reparented during the walk is omitted. That is a snapshot, not an error.
/// Callers that sum CPU across snapshots must tolerate processes appearing
/// and disappearing.
#[cfg(target_os = "macos")]
pub fn process_tree(root: u32) -> Option<Vec<ProcessTreeProcess>> {
    darwin_tree::snapshot(root)
}

#[cfg(target_os = "macos")]
pub(super) mod darwin_tree {
    use std::collections::{HashSet, VecDeque};
    use std::io::Error;
    use std::mem::{MaybeUninit, size_of};

    use super::{ProcessStartKey, ProcessTreeProcess};

    #[derive(Debug)]
    pub(in crate::process) enum ReadError {
        /// The process is gone. A descendant may be skipped; the root may not.
        Gone,
        /// The kernel refused or truncated the read. The snapshot is unusable.
        Unreadable,
    }

    pub(super) fn snapshot(root: u32) -> Option<Vec<ProcessTreeProcess>> {
        snapshot_with(root, read_process, child_pids, read_start)
    }

    // Kernel reads are injected so exit and reuse between them can be tested
    // without exhausting PIDs or generating host load.
    pub(in crate::process) fn snapshot_with(
        root: u32,
        mut read_process: impl FnMut(u32) -> Result<(ProcessTreeProcess, u32), ReadError>,
        mut child_pids: impl FnMut(u32) -> Result<Vec<u32>, ReadError>,
        mut read_start: impl FnMut(u32) -> Result<(u64, u32), ReadError>,
    ) -> Option<Vec<ProcessTreeProcess>> {
        let mut found = Vec::new();
        let mut seen = HashSet::from([root]);
        // Each queued child carries the identity of the parent that listed it.
        let mut queue = VecDeque::from([(root, None::<ProcessStartKey>)]);
        while let Some((pid, owner)) = queue.pop_front() {
            let is_root = owner.is_none();
            let (process, parent) = match read_process(pid) {
                Ok(read) => read,
                Err(ReadError::Gone) if !is_root => continue,
                Err(ReadError::Gone | ReadError::Unreadable) => return None,
            };
            if let Some(owner) = owner {
                // A listed PID may now name an unrelated process, and a parent
                // PID alone also matches a replacement of that parent.
                if parent != owner.pid {
                    continue;
                }
                match read_start(owner.pid) {
                    Ok((start, _)) if start == owner.starttime => {}
                    Ok(_) | Err(ReadError::Gone) => continue,
                    Err(ReadError::Unreadable) => return None,
                }
            }
            found.push(process);
            let children = match child_pids(pid) {
                Ok(children) => children,
                Err(ReadError::Gone) if !is_root => continue,
                Err(ReadError::Gone | ReadError::Unreadable) => return None,
            };
            // The listing names the children of whichever process held `pid`
            // at that moment. Walk them only if it was still the one measured,
            // in the same place in the tree.
            match read_start(pid) {
                Ok(current) if current == (process.key.starttime, parent) => {}
                Ok(_) | Err(ReadError::Gone) if !is_root => continue,
                Ok(_) | Err(ReadError::Gone | ReadError::Unreadable) => return None,
            }
            for child in children {
                if seen.insert(child) {
                    queue.push_back((child, Some(process.key)));
                }
            }
        }
        Some(found)
    }

    fn read_process(pid: u32) -> Result<(ProcessTreeProcess, u32), ReadError> {
        read_process_with(pid, read_start, read_rusage)
    }

    pub(in crate::process) fn read_process_with(
        pid: u32,
        mut read_start: impl FnMut(u32) -> Result<(u64, u32), ReadError>,
        mut read_rusage: impl FnMut(u32) -> Result<(u64, u64), ReadError>,
    ) -> Result<(ProcessTreeProcess, u32), ReadError> {
        let before = read_start(pid)?;
        let (cpu_time_ns, phys_footprint) = read_rusage(pid)?;
        // Bind usage to one identity across both calls. Reparenting means the
        // process left the tree while measured and is omitted like an exit.
        if read_start(pid)? != before {
            return Err(ReadError::Gone);
        }
        Ok((
            ProcessTreeProcess {
                key: ProcessStartKey {
                    pid,
                    starttime: before.0,
                },
                cpu_time_ns,
                phys_footprint,
            },
            before.1,
        ))
    }

    fn read_start(pid: u32) -> Result<(u64, u32), ReadError> {
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let expected =
            i32::try_from(size_of::<libc::proc_bsdinfo>()).map_err(|_| ReadError::Unreadable)?;
        clear_errno();
        // Safety: `info` points to `expected` writable bytes and is read only
        // after libproc reports that it filled the complete structure.
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
            return Err(classify_errno());
        }
        // Safety: the full structure was written above.
        let info = unsafe { info.assume_init() };
        let starttime = (info.pbi_start_tvsec << 20) | info.pbi_start_tvusec.min((1 << 20) - 1);
        Ok((starttime, info.pbi_ppid))
    }

    /// Own CPU time and physical footprint. `ri_child_*` is excluded: it
    /// repeats CPU already sampled from children that have been waited for.
    fn read_rusage(pid: u32) -> Result<(u64, u64), ReadError> {
        let mut info = MaybeUninit::<libc::rusage_info_v4>::zeroed();
        clear_errno();
        // Safety: `info` is a writable `rusage_info_v4`. `proc_pid_rusage`
        // takes `rusage_info_t *` (`void **` in the libc binding); passing
        // the struct address is the libproc contract for this flavor.
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        if rc != 0 {
            return Err(classify_errno());
        }
        // Safety: a zero return writes the flavor's structure.
        let info = unsafe { info.assume_init() };
        Ok((
            info.ri_user_time.saturating_add(info.ri_system_time),
            info.ri_phys_footprint,
        ))
    }

    /// Direct children of `ppid`. A full buffer is treated as truncated and
    /// retried. `proc_listchildpids` reports a failure as a zero count and
    /// leaves `errno` set, the same shape as a process with no children, so
    /// `errno` is cleared before the call.
    fn child_pids(ppid: u32) -> Result<Vec<u32>, ReadError> {
        let mut slots = 32usize;
        for _ in 0..6 {
            let mut pids = vec![libc::pid_t::from(0); slots];
            let bytes = i32::try_from(pids.len().saturating_mul(size_of::<libc::pid_t>()))
                .map_err(|_| ReadError::Unreadable)?;
            clear_errno();
            // Safety: `pids` is writable for exactly `bytes` bytes.
            let count = unsafe {
                libc::proc_listchildpids(ppid as libc::pid_t, pids.as_mut_ptr().cast(), bytes)
            };
            if count < 0 {
                return Err(classify_errno());
            }
            if count == 0 {
                return if errno_raw() == 0 {
                    Ok(Vec::new())
                } else {
                    Err(classify_errno())
                };
            }
            let count = usize::try_from(count).map_err(|_| ReadError::Unreadable)?;
            if count < pids.len() {
                return Ok(pids[..count]
                    .iter()
                    .filter_map(|pid| u32::try_from(*pid).ok())
                    .filter(|pid| *pid > 0)
                    .collect());
            }
            slots = slots.saturating_mul(2);
        }
        Err(ReadError::Unreadable)
    }

    fn clear_errno() {
        // Safety: `__error` is the calling thread's errno cell.
        unsafe { *libc::__error() = 0 };
    }

    fn errno_raw() -> i32 {
        Error::last_os_error().raw_os_error().unwrap_or(0)
    }

    fn classify_errno() -> ReadError {
        match errno_raw() {
            libc::ESRCH | libc::ENOENT => ReadError::Gone,
            _ => ReadError::Unreadable,
        }
    }
}
