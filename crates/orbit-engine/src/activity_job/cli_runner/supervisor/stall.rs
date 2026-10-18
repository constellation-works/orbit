//! Stop a provider whose process tree has produced no output and used no CPU.
//!
//! The wall-clock deadline still bounds a run that is making progress. This
//! watch ends one that is not: stdout and stderr byte counts and the live
//! tree's CPU time have all stayed flat for the configured window. An
//! unreadable sample is not evidence of idleness, and a drop in the CPU total
//! (a descendant reparented away) starts the window over.
//!
//! A provider blocked in the build-budget queue is idle in both signals, so
//! the window applies to it too. The failure is transient and the usual retry
//! path runs.

use std::time::{Duration, Instant};

use orbit_common::process::supervisor::ProcessStopCause;

/// Stop reason the supervisor records when this watch ends the run.
pub(super) const STALL_STOP_REASON: &str = "stalled";

const MIN_SAMPLE: Duration = Duration::from_millis(50);
const MAX_SAMPLE: Duration = Duration::from_secs(10);

pub(super) struct StallWatch {
    window: Duration,
    interval: Duration,
    root: u32,
    flat_since: Option<Instant>,
    last_cpu: Option<u64>,
    last_bytes: u64,
}

impl StallWatch {
    pub(super) fn new(window: Duration, root: u32) -> Self {
        let quarter = window / 4;
        Self {
            window,
            interval: quarter.clamp(MIN_SAMPLE, MAX_SAMPLE),
            root,
            flat_since: None,
            last_cpu: None,
            last_bytes: 0,
        }
    }

    pub(super) fn sample_interval(&self) -> Duration {
        self.interval
    }

    /// `None` keeps the run. The first readable sample is a baseline and
    /// cannot trip the watch.
    pub(super) fn poll(
        &mut self,
        stdout_bytes: usize,
        stderr_bytes: usize,
    ) -> Option<ProcessStopCause> {
        let Some(cpu) = sample_tree_cpu(self.root) else {
            self.flat_since = None;
            self.last_cpu = None;
            return None;
        };
        let bytes = u64::try_from(stdout_bytes.saturating_add(stderr_bytes)).unwrap_or(u64::MAX);
        let now = Instant::now();
        let changed =
            self.flat_since.is_none() || self.last_cpu != Some(cpu) || self.last_bytes != bytes;
        if changed {
            self.last_cpu = Some(cpu);
            self.last_bytes = bytes;
            self.flat_since = Some(now);
            return None;
        }
        let since = self.flat_since.unwrap_or(now);
        let flat_for = now.checked_duration_since(since).unwrap_or_default();
        (flat_for >= self.window).then_some(ProcessStopCause::Requested {
            reason: STALL_STOP_REASON,
        })
    }
}

/// Absolute CPU of the live tree rooted at `root`.
///
/// The unit is opaque and only has to grow when the tree runs: Linux clock
/// ticks, or macOS nanoseconds. `None` when the tree cannot be read. A
/// per-snapshot sum of own time plus reaped-child time over the live members
/// counts a busy descendant of an idle parent and does not count a reaped
/// child twice.
fn sample_tree_cpu(root: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_tree_cpu(root)
    }
    #[cfg(target_os = "macos")]
    {
        macos_tree_cpu(root)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        None
    }
}

#[cfg(target_os = "linux")]
fn linux_tree_cpu(root: u32) -> Option<u64> {
    use std::collections::HashSet;

    const MAX_MEMBERS: usize = 4096;
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if seen.len() > MAX_MEMBERS {
            return None;
        }
        match read_cpu_ticks(pid) {
            CpuRead::Ticks(ticks) => total = total.saturating_add(ticks),
            CpuRead::Gone if pid == root => return None,
            CpuRead::Gone => continue,
            CpuRead::Unreadable => return None,
        }
        match child_pids(pid) {
            Ok(children) => stack.extend(children),
            Err(CpuRead::Gone) => {}
            Err(CpuRead::Ticks(_) | CpuRead::Unreadable) => return None,
        }
    }
    Some(total)
}

#[cfg(target_os = "linux")]
enum CpuRead {
    Ticks(u64),
    Gone,
    Unreadable,
}

#[cfg(target_os = "linux")]
fn read_cpu_ticks(pid: u32) -> CpuRead {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => parse_cpu_ticks(&stat).map_or(CpuRead::Unreadable, CpuRead::Ticks),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => CpuRead::Gone,
        Err(_) => CpuRead::Unreadable,
    }
}

/// `utime + stime + cutime + cstime` from `/proc/<pid>/stat`.
///
/// Fields after the last `)`: state, ppid, pgrp, session, tty_nr, tpgid,
/// flags, minflt, cminflt, majflt, cmajflt, then those four.
#[cfg(target_os = "linux")]
fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let (_, tail) = stat.rsplit_once(')')?;
    let mut fields = tail.split_whitespace();
    for _ in 0..11 {
        fields.next()?;
    }
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    let cutime: u64 = fields.next()?.parse().ok()?;
    let cstime: u64 = fields.next()?.parse().ok()?;
    Some(
        utime
            .saturating_add(stime)
            .saturating_add(cutime)
            .saturating_add(cstime),
    )
}

#[cfg(target_os = "linux")]
fn child_pids(pid: u32) -> Result<Vec<u32>, CpuRead> {
    let entries = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CpuRead::Gone);
        }
        Err(_) => return Err(CpuRead::Unreadable),
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("children");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(CpuRead::Unreadable),
        };
        for token in text.split_whitespace() {
            if let Ok(child) = token.parse::<u32>()
                && child > 0
            {
                children.push(child);
            }
        }
    }
    Ok(children)
}

#[cfg(target_os = "macos")]
fn macos_tree_cpu(root: u32) -> Option<u64> {
    let tree = orbit_common::process::ancestry::process_tree(root)?;
    let mut total = 0u64;
    for process in tree {
        total = total.saturating_add(process.cpu_time_ns);
        total = total.saturating_add(process.reaped_cpu_time_ns);
    }
    Some(total)
}
