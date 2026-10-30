//! Supervision of one build phase: one capped log, a wall-clock bound and a
//! build-directory size bound (§3.4, §4 resource exhaustion).

use std::collections::VecDeque;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::process::supervisor::{
    OutputSink, Periodic, ProcessStopCause, SinkVerdict, Stdin, Stream, SupervisedChild,
    Supervision, Termination,
};

use super::{BuildPhaseEnd, BuildPhaseRequest};

/// The most build output a host keeps: the first and last half of this many
/// bytes, with the middle elided (§3.6).
pub const PLUGIN_BUILD_LOG_CAP_BYTES: usize = 1024 * 1024;

/// How often the build directory is measured. The cap can be overshot by
/// what a build writes in one interval.
const SIZE_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long output is drained after the phase's process exits. A descendant
/// that escaped the process group can hold the pipe open; it is not waited
/// for.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const BUILD_DIR_CAP_REASON: &str = "build directory cap exceeded";

/// The build log of one install: both phases' stdout and stderr, and Orbit's
/// own phase notes, capped at a head and a tail.
#[derive(Debug)]
pub struct BuildLog {
    half: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    elided: u64,
}

impl Default for BuildLog {
    fn default() -> Self {
        Self::with_cap(PLUGIN_BUILD_LOG_CAP_BYTES)
    }
}

impl BuildLog {
    /// A log keeping at most `cap` bytes of output.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            half: cap / 2,
            head: Vec::new(),
            tail: VecDeque::new(),
            elided: 0,
        }
    }

    /// Append Orbit's own line (a phase header or outcome).
    pub fn note(&mut self, line: &str) {
        self.push(format!("[orbit] {line}\n").as_bytes());
    }

    pub fn push(&mut self, mut bytes: &[u8]) {
        let head_room = self.half.saturating_sub(self.head.len());
        if self.tail.is_empty() && head_room > 0 {
            let taken = head_room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..taken]);
            bytes = &bytes[taken..];
        }
        self.tail.extend(bytes);
        let excess = self.tail.len().saturating_sub(self.half);
        if excess > 0 {
            self.tail.drain(..excess);
            self.elided += excess as u64;
        }
    }

    /// The kept bytes: the head, a marker naming how much was elided, and
    /// the tail.
    pub fn render(&self) -> Vec<u8> {
        let mut out = self.head.clone();
        if self.elided > 0 {
            out.extend_from_slice(
                format!(
                    "\n[orbit] … {} bytes of build output elided …\n",
                    self.elided
                )
                .as_bytes(),
            );
        }
        out.extend(self.tail.iter());
        out
    }
}

/// Spawn `command` (the sandbox wrapper) and supervise it. `guard` holds what
/// the wrapper needs until it ends (a ruleset descriptor).
pub(super) fn run<G>(
    command: Command,
    guard: G,
    request: &BuildPhaseRequest<'_>,
    log: &mut BuildLog,
) -> Result<BuildPhaseEnd, OrbitError> {
    let phase = Phase {
        env: request.env,
        cwd: request.cwd,
        timeout: request.timeout,
        size_root: request.sandbox.build_dir,
        size_cap: request.build_dir_cap_bytes,
        stdin: None,
        stdout_cap: None,
        grace: crate::supervision::TERMINATION_GRACE_PERIOD,
    };
    Ok(run_phase(command, guard, &phase, log)?.end)
}

/// One supervised sandbox start: the wrapper's environment and working
/// directory, the bounds that kill its process group, and its stdio.
pub(super) struct Phase<'a> {
    pub(super) env: &'a [(String, String)],
    pub(super) cwd: &'a Path,
    pub(super) timeout: Duration,
    /// The directory whose size is bounded, polled every second.
    pub(super) size_root: &'a Path,
    pub(super) size_cap: u64,
    /// Bytes written to the child's stdin, then closed; `None` is no stdin.
    pub(super) stdin: Option<Vec<u8>>,
    /// Keep stdout apart from the log, up to this many bytes; `None` sends
    /// stdout to the log only.
    pub(super) stdout_cap: Option<usize>,
    /// How long a stopped group gets between `SIGTERM` and `SIGKILL`.
    pub(super) grace: Duration,
}

/// What one supervised start produced.
pub(super) struct PhaseRun {
    pub(super) end: BuildPhaseEnd,
    /// The stdout kept apart from the log (empty without `stdout_cap`).
    #[cfg(target_os = "linux")]
    pub(super) stdout: Vec<u8>,
    /// Stdout went past `stdout_cap`; what is kept is its head.
    #[cfg(target_os = "linux")]
    pub(super) stdout_over_cap: bool,
}

pub(super) fn run_phase<G>(
    mut command: Command,
    guard: G,
    request: &Phase<'_>,
    log: &mut BuildLog,
) -> Result<PhaseRun, OrbitError> {
    command
        .env_clear()
        .envs(request.env.iter().map(|(key, value)| (key, value)))
        .current_dir(request.cwd)
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` and `setrlimit` are async-signal-safe and touch
        // only the forked child's own session, group and limits.
        unsafe {
            command.pre_exec(|| {
                // A fresh session also creates the process group whose id
                // is the child's PID, which the termination supervisor kills.
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &none) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = SupervisedChild::adopt(command.spawn().map_err(|error| {
        OrbitError::Execution(format!("failed to start the build sandbox: {error}"))
    })?);
    // Both pipe workers append to the same capped log, including prior phase
    // output and Orbit's notes. Restore it even if supervision returns an error.
    let shared = Arc::new(Mutex::new(std::mem::take(log)));
    let mut check_size = || {
        tree_exceeds(request.size_root, request.size_cap).then_some(ProcessStopCause::Requested {
            reason: BUILD_DIR_CAP_REASON,
        })
    };
    let stdout: Box<dyn OutputSink> = match request.stdout_cap {
        Some(cap) => Box::new(CappedStdoutSink {
            log: Arc::clone(&shared),
            kept: Vec::new(),
            cap,
            over_cap: false,
        }),
        None => Box::new(BuildLogSink(Arc::clone(&shared))),
    };
    let outcome = child.supervise(Supervision {
        stdin: match &request.stdin {
            Some(bytes) => Stdin::Payload(bytes.clone()),
            None => Stdin::Null,
        },
        stdout: Stream::Sink(stdout),
        stderr: Stream::Sink(Box::new(BuildLogSink(Arc::clone(&shared)))),
        timeout: Some(request.timeout),
        clock: None,
        extendable_deadline: None,
        termination: Termination::Grace(request.grace),
        cancelled: None,
        intercept_signals: false,
        drain_budget: DRAIN_GRACE,
        cap_drain_at_deadline: false,
        require_eof: false,
        stopped_descendants: None,
        on_stopped_descendant: None,
        on_spawn: None,
        every: Some(Periodic {
            interval: SIZE_POLL_INTERVAL,
            run: &mut check_size,
        }),
    });
    *log = std::mem::take(&mut *shared.lock().unwrap_or_else(|error| error.into_inner()));
    // The ruleset descriptor remains live until the child and its pipes end.
    drop(guard);
    let outcome = outcome?;
    let mut end = if outcome.timed_out {
        BuildPhaseEnd::TimedOut
    } else if outcome.stop_causes.contains(&ProcessStopCause::Requested {
        reason: BUILD_DIR_CAP_REASON,
    }) {
        BuildPhaseEnd::BuildDirCapExceeded
    } else if let Some(status) = outcome.stop_causes.iter().find_map(|cause| match cause {
        ProcessStopCause::TerminationStatus { status } => Some(*status),
        _ => None,
    }) {
        exit_end(status)
    } else {
        BuildPhaseEnd::Exited(outcome.exit_code.unwrap_or(-1))
    };
    // A phase can finish between polls. Do not accept a build whose
    // directory is already over the cap.
    if matches!(end, BuildPhaseEnd::Exited(_)) && tree_exceeds(request.size_root, request.size_cap)
    {
        end = BuildPhaseEnd::BuildDirCapExceeded;
    }
    // Only Linux script runs consume a separate stdout answer. Build phases
    // on every platform retain their output in the shared log instead.
    #[cfg(target_os = "linux")]
    let (stdout, stdout_over_cap) = {
        // The sink keeps one byte past the cap to mark an overflow.
        let mut stdout = outcome.stdout;
        let over_cap = request.stdout_cap.is_some_and(|cap| stdout.len() > cap);
        if let Some(cap) = request.stdout_cap {
            stdout.truncate(cap);
        }
        (stdout, over_cap)
    };
    Ok(PhaseRun {
        end,
        #[cfg(target_os = "linux")]
        stdout,
        #[cfg(target_os = "linux")]
        stdout_over_cap,
    })
}

/// Stdout of a script: every byte also goes to the shared log, and the
/// first `cap` bytes are kept apart as the script's answer. One byte past
/// the cap is kept to mark an overflow; the child keeps running and its
/// pipe stays open.
struct CappedStdoutSink {
    log: Arc<Mutex<BuildLog>>,
    kept: Vec<u8>,
    cap: usize,
    over_cap: bool,
}

impl OutputSink for CappedStdoutSink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        self.log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(bytes);
        if !self.over_cap {
            let room = (self.cap + 1).saturating_sub(self.kept.len());
            let taken = room.min(bytes.len());
            self.kept.extend_from_slice(&bytes[..taken]);
            self.over_cap = self.kept.len() > self.cap;
        }
        SinkVerdict::Continue
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        self.kept
    }
}

/// Each stream shares one log; reaching its cap elides output without
/// stopping the build or closing either pipe.
struct BuildLogSink(Arc<Mutex<BuildLog>>);

impl OutputSink for BuildLogSink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(bytes);
        SinkVerdict::Continue
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        Vec::new()
    }
}

fn exit_end(status: std::process::ExitStatus) -> BuildPhaseEnd {
    if let Some(code) = status.code() {
        return BuildPhaseEnd::Exited(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return BuildPhaseEnd::Signaled(signal);
        }
    }
    BuildPhaseEnd::Exited(-1)
}

/// Whether the apparent size of everything under `root` exceeds `cap`,
/// without following links. Stops counting once the cap is passed, and
/// treats traversal errors as exceeding the cap because the size is unknown.
/// A path that vanishes mid-walk (a live build deleting temporary files) is
/// not an error: nothing is hidden behind a `NotFound`.
pub(super) fn tree_exceeds(root: &Path, cap: u64) -> bool {
    let mut total: u64 = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return true,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            // Do not follow a symlink out of the build directory (or back to
            // an ancestor). Besides reading host metadata, a self-referential
            // directory link would make this traversal loop forever and
            // defeat the phase timeout.
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return true,
            };
            let file_type = metadata.file_type();
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total = total.saturating_add(metadata.len());
                if total > cap {
                    return true;
                }
            }
        }
    }
    false
}
