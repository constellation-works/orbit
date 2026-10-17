//! Process ancestry keyed by pid plus start time, so a recorded child can be
//! recognised after it unsets its environment.
//!
//! A pid alone is reused. Pairing it with the kernel start time is what lets
//! a later `orbit tool run` (or MCP server) decide it is still the process the
//! host launched as a plugin backend, or a descendant of one.

use std::collections::HashMap;

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
/// Own CPU time and the CPU time of reaped children are kept apart. A child
/// that a snapshot saw alive reappears in its parent's reaped total once it
/// is waited for, so summing both naively counts it twice; [`TreeCpuAccount`]
/// combines them without that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessTreeProcess {
    pub key: ProcessStartKey,
    /// The tree member that listed this process as its child; `None` for the
    /// root.
    pub parent: Option<ProcessStartKey>,
    /// Own user plus system CPU time, in nanoseconds.
    pub cpu_time_ns: u64,
    /// User plus system CPU time of the children this process has waited
    /// for, in nanoseconds. A reaped child brings what it had itself reaped,
    /// so this covers every waited-for descendant (`ri_child_user_time` plus
    /// `ri_child_system_time`).
    pub reaped_cpu_time_ns: u64,
    /// Non-reclaimable physical footprint (`ri_phys_footprint`), in bytes.
    /// Resident size counts file-backed and shared pages, which overstates
    /// the tree the way `memory.current` does on Linux.
    pub phys_footprint: u64,
}

/// CPU charged to a process tree across snapshots, including descendants
/// that ran and were reaped between them.
///
/// A process's total is its own CPU plus the CPU of the children it has
/// reaped. A process in both snapshots is charged the growth of its total,
/// and a newcomer its whole total. A descendant that started and was reaped
/// between snapshots therefore arrives through its parent's reaped total
/// even though no snapshot saw it. The first snapshot is a baseline and
/// charges nothing.
///
/// A descendant that a snapshot did see arrives the same way when reaped, so
/// its last observed total was already charged. Each one that leaves the
/// tree is asked of the kernel (`locate`):
///
/// - Gone, or its pid now names another process: it exited. Its last
///   observed total, plus whatever it was still owed, becomes owed by the
///   member that listed it. If that member left by exiting too, the debt
///   climbs to the nearest ancestor still in the tree, because a reaped
///   process's total carries what it had reaped. Growth of the owing
///   member's reaped total settles the debt instead of being charged, so the
///   walk reading a parent before its child's reap only shifts the charge to
///   a later snapshot.
/// - Alive under the same parent: the walk missed it. It stays tracked, so
///   it is neither charged again nor written off.
/// - Alive under another parent: it was reparented away. What it ran here
///   stays charged, and nothing in the tree will reap it, so it creates no
///   debt. Debt it was owed is dropped with it.
///
/// An exited process that has not been waited for stays listed and readable
/// as a zombie, so one that left the walk had already been reaped before
/// the walk reached it. When its owner reaped it, the reap shows in the
/// owner's reaped total in that snapshot or, if the walk read the owner
/// first, at the owner's next sighting. A debt therefore settles only at the
/// owner's sighting in the snapshot it climbed in and at the owner's next
/// sighting; what is left after that is dropped. Each climb restarts that
/// window, so a reap that lands one level up a sample later is still matched.
///
/// Some debts are never paid. A descendant reaped outside the tree never
/// shows in a member's reaped total: one whose parent exited without waiting
/// for it (a build killed under a shell that the agent then reaps, or a
/// dropped child handle whose parent exits after it), one whose parent sets
/// `SA_NOCLDWAIT`, or one reparented and exited within one sample interval.
/// Until the window closes, its debt absorbs the owner's unrelated reaped
/// growth, so the tree undercounts by at most the owner's reaped growth over
/// those two sightings, and never by more than the debt itself.
#[derive(Debug, Default)]
pub struct TreeCpuAccount {
    tracked: HashMap<ProcessStartKey, Tracked>,
    baseline: bool,
    cumulative_ns: u64,
}

#[derive(Debug, Clone, Copy)]
struct Tracked {
    parent: Option<ProcessStartKey>,
    total_ns: u64,
    reaped_ns: u64,
    /// Last observed totals of descendants that exited under this process
    /// and that its reaped total has not absorbed yet. Whatever its next
    /// sighting does not settle is dropped.
    owed_ns: u64,
}

impl TreeCpuAccount {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charge one snapshot and return the nanoseconds it added. `locate`
    /// reads a pid's current start key and parent pid, as
    /// [`process_start_and_parent`] does, for the processes that left.
    pub fn observe(
        &mut self,
        current: &[ProcessTreeProcess],
        mut locate: impl FnMut(u32) -> Option<(ProcessStartKey, u32)>,
    ) -> u64 {
        let mut previous = std::mem::take(&mut self.tracked);
        let mut next: HashMap<ProcessStartKey, Tracked> = current
            .iter()
            .map(|process| {
                let owed_ns = previous
                    .get(&process.key)
                    .map_or(0, |before| before.owed_ns);
                let tracked = Tracked {
                    parent: process.parent,
                    total_ns: process
                        .cpu_time_ns
                        .saturating_add(process.reaped_cpu_time_ns),
                    reaped_ns: process.reaped_cpu_time_ns,
                    owed_ns,
                };
                (process.key, tracked)
            })
            .collect();

        let departed: Vec<ProcessStartKey> = previous
            .keys()
            .filter(|key| !next.contains_key(key))
            .copied()
            .collect();
        let mut exited = HashMap::new();
        for key in departed {
            let Some(tracked) = previous.remove(&key) else {
                continue;
            };
            match locate(key.pid) {
                Some((now, ppid)) if now == key => {
                    if tracked.parent.is_some_and(|parent| parent.pid == ppid) {
                        next.insert(key, tracked);
                    }
                }
                _ => {
                    exited.insert(key, tracked);
                }
            }
        }
        // Debt that climbs in this snapshot, kept apart from older debt
        // because it may still settle at the owner's next sighting.
        let mut climbed: HashMap<ProcessStartKey, u64> = HashMap::new();
        for tracked in exited.values() {
            let owed = tracked.total_ns.saturating_add(tracked.owed_ns);
            let mut parent = tracked.parent;
            // Each hop moves to an exited ancestor; the bound only guards
            // against a malformed parent chain.
            for _ in 0..=exited.len() {
                let Some(key) = parent else { break };
                if next.contains_key(&key) {
                    let debt = climbed.entry(key).or_default();
                    *debt = debt.saturating_add(owed);
                    break;
                }
                parent = exited.get(&key).and_then(|ancestor| ancestor.parent);
            }
        }

        let mut charged = 0u64;
        for process in current {
            let Some(now) = next.get_mut(&process.key) else {
                continue;
            };
            let (grown, reaped) = match previous.get(&process.key) {
                Some(before) => (
                    now.total_ns.saturating_sub(before.total_ns),
                    now.reaped_ns.saturating_sub(before.reaped_ns),
                ),
                None => (now.total_ns, now.reaped_ns),
            };
            // Older debt settles first, and this was its last chance: its
            // remainder is dropped. New debt waits one more sighting.
            let due = now.owed_ns;
            let fresh = climbed.remove(&process.key).unwrap_or(0);
            let settled = due.saturating_add(fresh).min(reaped);
            now.owed_ns = fresh.saturating_sub(settled.saturating_sub(due));
            charged = charged.saturating_add(grown.saturating_sub(settled));
        }
        // A member the walk missed was not read, so debt that climbed to it
        // waits for its next sighting.
        for (key, owed) in climbed {
            if let Some(owner) = next.get_mut(&key) {
                owner.owed_ns = owner.owed_ns.saturating_add(owed);
            }
        }
        self.tracked = next;
        if !self.baseline {
            self.baseline = true;
            return 0;
        }
        self.cumulative_ns = self.cumulative_ns.saturating_add(charged);
        charged
    }

    /// Nanoseconds charged since the baseline.
    pub fn cumulative_ns(&self) -> u64 {
        self.cumulative_ns
    }
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
        // Each queued child carries the start identity and observed parent PID
        // of the process that listed it, so reparenting invalidates ownership.
        let mut queue = VecDeque::from([(root, None::<(ProcessStartKey, u32)>)]);
        while let Some((pid, owner)) = queue.pop_front() {
            let is_root = owner.is_none();
            let (mut process, parent) = match read_process(pid) {
                Ok(read) => read,
                Err(ReadError::Gone) if !is_root => continue,
                Err(ReadError::Gone | ReadError::Unreadable) => return None,
            };
            if let Some((owner, owner_parent)) = owner {
                // A listed PID may now name an unrelated process, and a parent
                // PID alone also matches a replacement of that parent. The
                // listing parent may also have left the tree since queueing.
                if parent != owner.pid {
                    continue;
                }
                match read_start(owner.pid) {
                    Ok(current) if current == (owner.starttime, owner_parent) => {}
                    Ok(_) | Err(ReadError::Gone) => continue,
                    Err(ReadError::Unreadable) => return None,
                }
            }
            process.parent = owner.map(|(owner, _)| owner);
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
                    queue.push_back((child, Some((process.key, parent))));
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
        mut read_rusage: impl FnMut(u32) -> Result<(u64, u64, u64), ReadError>,
    ) -> Result<(ProcessTreeProcess, u32), ReadError> {
        let before = read_start(pid)?;
        let (cpu_time_ns, reaped_cpu_time_ns, phys_footprint) = read_rusage(pid)?;
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
                parent: None,
                cpu_time_ns,
                reaped_cpu_time_ns,
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

    /// Own CPU time, reaped children's CPU time and physical footprint. The
    /// `ri_child_*` counters accumulate the children's `ri_user_time` and
    /// `ri_system_time`, so they share the own counters' Mach units.
    fn read_rusage(pid: u32) -> Result<(u64, u64, u64), ReadError> {
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
        let timebase = read_timebase();
        Ok((
            mach_cpu_ns(info.ri_user_time, info.ri_system_time, timebase)?,
            mach_cpu_ns(info.ri_child_user_time, info.ri_child_system_time, timebase)?,
            info.ri_phys_footprint,
        ))
    }

    // libc recommends mach2 for these bindings; the underlying kernel API
    // remains supported. Keep the existing libc dependency for this query.
    #[allow(deprecated)]
    fn read_timebase() -> Option<(u32, u32)> {
        let mut timebase = MaybeUninit::<libc::mach_timebase_info_data_t>::zeroed();
        // Safety: the pointer names a writable timebase structure, read only
        // after the kernel reports success. These counters are Mach absolute
        // time (task power totals), not nanoseconds on every Mac.
        let rc = unsafe { libc::mach_timebase_info(timebase.as_mut_ptr()) };
        if rc == libc::KERN_SUCCESS {
            // Safety: the successful call initialized the structure.
            let timebase = unsafe { timebase.assume_init() };
            Some((timebase.numer, timebase.denom))
        } else {
            None
        }
    }

    /// Convert a user/system pair of absolute ticks. Widen before both
    /// addition and multiplication, then truncate sub-nanoseconds once. An
    /// unavailable timebase or an unrepresentable result invalidates the
    /// sample.
    pub(in crate::process) fn mach_cpu_ns(
        user: u64,
        system: u64,
        timebase: Option<(u32, u32)>,
    ) -> Result<u64, ReadError> {
        let (numer, denom) = timebase.ok_or(ReadError::Unreadable)?;
        if numer == 0 || denom == 0 {
            return Err(ReadError::Unreadable);
        }
        let ns = (u128::from(user) + u128::from(system)) * u128::from(numer) / u128::from(denom);
        u64::try_from(ns).map_err(|_| ReadError::Unreadable)
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
