//! Find and end descendants of a supervised child that stay stopped.
//!
//! A shell an agent runs can stop itself (state `T`): an interactive probe
//! such as `bash -i` in a sandbox without a controlling terminal, or a
//! `kill -STOP $$`. Its parent then waits on it until the supervisor's wall
//! clock ends, hours later, and every liveness probe reports the agent alive
//! the whole time [F2026-10-156]. A stopped `timeout` never fires its alarm.
//!
//! [`StoppedDescendantWatch`] samples the supervised child's process tree and
//! ends, with `SIGKILL`, a descendant that has stayed stopped across the whole
//! threshold. Its parent's wait then returns and the agent carries on.
//!
//! What it never touches:
//! - the supervised child itself, and every descendant while the child is
//!   stopped too (an operator stopped the whole group);
//! - a process stopped for less than the threshold, or not re-read stopped at
//!   least [`MIN_STOPPED_REREADS`] times after it was first seen;
//! - a running, sleeping or traced (`t`) process, however idle;
//! - a process outside the child's tree, or one whose pid now names a process
//!   with a different start time.
//!
//! Linux only: `/proc` answers parent, state and start time for every process
//! in one scan. On other hosts the watch never reports.

use std::time::{Duration, Instant};

/// Overrides [`DEFAULT_STOPPED_DESCENDANT_THRESHOLD`], in milliseconds.
pub const STOPPED_DESCENDANT_THRESHOLD_ENV: &str = "ORBIT_STOPPED_DESCENDANT_THRESHOLD_MS";

/// How long a descendant must stay stopped before it is ended.
///
/// A debugger stop reads `t`, not `T`, so it never counts. A job-control
/// stop needs a terminal, and supervised children run without one, so a `T`
/// descendant is one that stopped itself or was stopped by someone else. The
/// incident stop never ended on its own (2h28m). Ten minutes is far past any
/// pause an operator makes by hand and well inside an agent's wall clock.
pub const DEFAULT_STOPPED_DESCENDANT_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// Times a stopped descendant must be read stopped again after it was first
/// seen before it can be ended.
pub const MIN_STOPPED_REREADS: u32 = 2;

/// Upper bound on the time between two samples of the tree.
const MAX_SAMPLE_INTERVAL: Duration = Duration::from_secs(15);
const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(10);
/// Bytes of a descendant's command line kept in its report.
#[cfg(target_os = "linux")]
const COMMAND_LIMIT_BYTES: usize = 256;

/// The threshold supervisors use: [`STOPPED_DESCENDANT_THRESHOLD_ENV`] when it
/// holds a positive number of milliseconds, otherwise the default.
pub fn stopped_descendant_threshold() -> Duration {
    std::env::var(STOPPED_DESCENDANT_THRESHOLD_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_STOPPED_DESCENDANT_THRESHOLD)
}

/// A descendant the watch found stopped past the threshold, and what ending
/// it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoppedDescendant {
    pub pid: u32,
    /// Versioned start identity token for `pid`, read before it was signalled.
    pub pid_start_time: Option<String>,
    /// Its command line, bounded and redacted.
    pub command: String,
    /// How long it was observed stopped: from the first sample that saw it
    /// stopped, so it was stopped at least this long.
    pub stopped_for: Duration,
    /// `None` when `SIGKILL` was delivered; otherwise why it could not be.
    pub end_error: Option<String>,
}

impl StoppedDescendant {
    /// Whether the descendant was signalled.
    pub fn ended(&self) -> bool {
        self.end_error.is_none()
    }

    /// One line naming the process, its command and how long it was stopped.
    pub fn describe(&self) -> String {
        let outcome = match &self.end_error {
            None => "ended".to_string(),
            Some(error) => format!("could not end ({error})"),
        };
        format!(
            "stopped descendant pid={} command=`{}` stopped for at least {}s: {outcome}",
            self.pid,
            self.command,
            self.stopped_for.as_secs()
        )
    }
}

/// Samples one supervised child's process tree; see the module docs.
#[derive(Debug)]
pub struct StoppedDescendantWatch {
    threshold: Duration,
    sample_interval: Duration,
    next_sample: Instant,
    /// First sighting and re-read count per stopped descendant, keyed by pid
    /// and start ticks so a reused pid starts over.
    #[cfg(target_os = "linux")]
    stopped: std::collections::HashMap<(u32, u64), Sighting>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct Sighting {
    since: Instant,
    rereads: u32,
}

impl StoppedDescendantWatch {
    /// A watch that ends a descendant stopped for `threshold`. It samples
    /// often enough to re-read a stopped process several times within it.
    pub fn new(threshold: Duration) -> Self {
        let sample_interval = (threshold / 4).clamp(MIN_SAMPLE_INTERVAL, MAX_SAMPLE_INTERVAL);
        Self {
            threshold,
            sample_interval,
            next_sample: Instant::now() + sample_interval,
            #[cfg(target_os = "linux")]
            stopped: std::collections::HashMap::new(),
        }
    }

    /// When the next sample is due. A supervisor that blocks in a wait ends
    /// the wait here and calls [`Self::poll`].
    pub fn next_sample_at(&self) -> Instant {
        self.next_sample
    }

    /// Sample `root`'s tree when a sample is due, and end every descendant
    /// that has stayed stopped across the threshold. Returns what it ended or
    /// failed to end; empty when nothing was due.
    ///
    /// `root` must be the supervisor's own unreaped child, so its pid cannot
    /// name another process.
    pub fn poll(&mut self, root: u32) -> Vec<StoppedDescendant> {
        let now = Instant::now();
        if now < self.next_sample {
            return Vec::new();
        }
        self.next_sample = now + self.sample_interval;
        #[cfg(target_os = "linux")]
        {
            self.sample_linux(root, now)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (root, self.threshold);
            Vec::new()
        }
    }

    #[cfg(target_os = "linux")]
    fn sample_linux(&mut self, root: u32, now: Instant) -> Vec<StoppedDescendant> {
        let table = linux::process_table();
        // A stopped (or vanished) root means the whole job was stopped from
        // outside: leave it alone, and start every count over once it resumes.
        if table.get(&root).is_none_or(|stat| stat.state == 'T') {
            self.stopped.clear();
            return Vec::new();
        }
        let mut still_stopped = std::collections::HashMap::new();
        let mut due = Vec::new();
        for (pid, stat) in linux::descendants(&table, root) {
            if stat.state != 'T' {
                continue;
            }
            let key = (pid, stat.start_ticks);
            let sighting = match self.stopped.get(&key) {
                Some(seen) => Sighting {
                    since: seen.since,
                    rereads: seen.rereads.saturating_add(1),
                },
                None => Sighting {
                    since: now,
                    rereads: 0,
                },
            };
            let stopped_for = now.saturating_duration_since(sighting.since);
            if sighting.rereads >= MIN_STOPPED_REREADS && stopped_for >= self.threshold {
                due.push((pid, stat.start_ticks, stopped_for));
            } else {
                still_stopped.insert(key, sighting);
            }
        }
        // Anything not re-read stopped this time resumed or exited.
        self.stopped = still_stopped;
        due.into_iter()
            .filter_map(|(pid, start_ticks, stopped_for)| {
                linux::end_stopped(pid, start_ticks, stopped_for)
            })
            .collect()
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::time::Duration;

    use super::{COMMAND_LIMIT_BYTES, StoppedDescendant};
    use crate::process::identity::{
        LinuxProcessStat, linux_process_stat, process_start_identity_token,
    };
    use crate::security::redaction::{argv_redactor, redact_all};

    /// Every process `/proc` lists, by pid. A process that exits during the
    /// scan is simply missing.
    pub(super) fn process_table() -> HashMap<u32, LinuxProcessStat> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return HashMap::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .filter_map(|pid| Some((pid, linux_process_stat(pid)?)))
            .collect()
    }

    /// `root`'s descendants, excluding `root`, walked through parent pids.
    pub(super) fn descendants(
        table: &HashMap<u32, LinuxProcessStat>,
        root: u32,
    ) -> Vec<(u32, LinuxProcessStat)> {
        let mut children = HashMap::<u32, Vec<u32>>::new();
        for (pid, stat) in table {
            children.entry(stat.parent_pid).or_default().push(*pid);
        }
        let mut found = Vec::new();
        // Parent links form a tree, but a pid reused mid-scan could fake a
        // cycle; visit each pid once.
        let mut visited = HashSet::from([root]);
        let mut queue = VecDeque::from([root]);
        while let Some(parent) = queue.pop_front() {
            for child in children.get(&parent).into_iter().flatten() {
                if !visited.insert(*child) {
                    continue;
                }
                if let Some(stat) = table.get(child) {
                    found.push((*child, *stat));
                }
                queue.push_back(*child);
            }
        }
        found
    }

    /// `SIGKILL` the process `pid` names, only while it is still the stopped
    /// process that started at `start_ticks`. `None` when it is not (it
    /// resumed, exited, or its pid was reused): nothing was signalled and
    /// nothing is reported.
    pub(crate) fn end_stopped(
        pid: u32,
        start_ticks: u64,
        stopped_for: Duration,
    ) -> Option<StoppedDescendant> {
        let pid_t = libc::pid_t::try_from(pid).ok()?;
        // A pidfd pins the process: once it is open, `pid` cannot be reused
        // under it, so the check below holds for the signal that follows.
        // SAFETY: pidfd_open takes a pid and flags and returns a new fd.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid_t, 0) };
        let pidfd = if let Ok(fd) = i32::try_from(raw)
            && fd >= 0
        {
            // SAFETY: the kernel just returned this descriptor to us.
            Some(unsafe { OwnedFd::from_raw_fd(fd) })
        } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS) {
            // Pre-5.3 kernel. A stopped process cannot exit by itself, so its
            // pid stays its own between the check and `kill` unless someone
            // else kills it in that window.
            None
        } else {
            return None;
        };
        let current = linux_process_stat(pid)?;
        if current.start_ticks != start_ticks || current.state != 'T' {
            return None;
        }
        let pid_start_time = process_start_identity_token(pid);
        let command = command_line(pid);
        let delivered = match &pidfd {
            Some(fd) => {
                use std::os::fd::AsRawFd;
                // SAFETY: a valid pidfd, a signal number, no siginfo, no flags.
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                }
            }
            // SAFETY: signals the pid verified just above.
            None => libc::c_long::from(unsafe { libc::kill(pid_t, libc::SIGKILL) }),
        };
        let end_error = (delivered != 0).then(|| std::io::Error::last_os_error().to_string());
        Some(StoppedDescendant {
            pid,
            pid_start_time,
            command,
            stopped_for,
            end_error,
        })
    }

    /// The process's argv joined by spaces, or its `comm` when argv is empty,
    /// bounded and redacted: it is persisted and printed.
    fn command_line(pid: u32) -> String {
        let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let mut command = argv
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(String::from_utf8_lossy)
            .collect::<Vec<_>>()
            .join(" ");
        if command.is_empty() {
            command = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                .map(|comm| comm.trim().to_string())
                .unwrap_or_default();
        }
        if command.len() > COMMAND_LIMIT_BYTES {
            let end = crate::text::floor_char_boundary(&command, COMMAND_LIMIT_BYTES);
            command.truncate(end);
            command.push('…');
        }
        redact_all(&argv_redactor().apply_str(&command))
    }
}
