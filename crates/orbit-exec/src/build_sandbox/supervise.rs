//! Supervision of one build phase: one capped log, a wall-clock bound and a
//! build-directory size bound (§3.4, §4 resource exhaustion).

use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::{BuildPhaseEnd, BuildPhaseRequest};

/// The most build output a host keeps: the first and last half of this many
/// bytes, with the middle elided (§3.6).
pub const PLUGIN_BUILD_LOG_CAP_BYTES: usize = 1024 * 1024;

/// How often the build directory is measured. The cap can be overshot by
/// what a build writes in one interval.
const SIZE_POLL_INTERVAL: Duration = Duration::from_secs(1);
const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long output is drained after the phase's process exits. A descendant
/// that escaped the process group can hold the pipe open; it is not waited
/// for.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
const KILL_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Bound of the reader-to-supervisor channel, in chunks.
const OUTPUT_CHANNEL_CHUNKS: usize = 64;

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
/// the wrapper needs until it ends (a ruleset descriptor, a profile file).
pub(super) fn run<G>(
    mut command: Command,
    guard: G,
    request: &BuildPhaseRequest<'_>,
    log: &mut BuildLog,
) -> Result<BuildPhaseEnd, OrbitError> {
    command
        .env_clear()
        .envs(request.env.iter().map(|(key, value)| (key, value)))
        .current_dir(request.cwd)
        .stdin(Stdio::null())
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
    let mut child = command.spawn().map_err(|error| {
        OrbitError::Execution(format!("failed to start the build sandbox: {error}"))
    })?;
    let (sender, output) = mpsc::sync_channel::<Vec<u8>>(OUTPUT_CHANNEL_CHUNKS);
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|pipe| {
        let sender = sender.clone();
        std::thread::spawn(move || relay(pipe, &sender))
    })
    .collect();
    drop(sender);

    let started = Instant::now();
    let mut next_size_check = started + SIZE_POLL_INTERVAL;
    let mut end = loop {
        if let Ok(chunk) = output.recv_timeout(OUTPUT_POLL_INTERVAL) {
            log.push(&chunk);
        }
        let status = child
            .try_wait()
            .map_err(|error| OrbitError::Execution(format!("wait for the build: {error}")))?;
        if let Some(status) = status {
            break exit_end(status);
        }
        let now = Instant::now();
        if now.duration_since(started) >= request.timeout {
            stop(&mut child)?;
            break BuildPhaseEnd::TimedOut;
        }
        if now >= next_size_check {
            next_size_check = now + SIZE_POLL_INTERVAL;
            if tree_exceeds(request.sandbox.build_dir, request.build_dir_cap_bytes) {
                stop(&mut child)?;
                break BuildPhaseEnd::BuildDirCapExceeded;
            }
        }
    };
    // Whatever the phase left behind in its group goes with it.
    stop(&mut child)?;
    // The polling interval bounds how far an active writer can overshoot,
    // but a phase can also finish between polls. Do not accept a successful
    // build whose directory is already over the cap.
    if matches!(end, BuildPhaseEnd::Exited(_))
        && tree_exceeds(request.sandbox.build_dir, request.build_dir_cap_bytes)
    {
        end = BuildPhaseEnd::BuildDirCapExceeded;
    }
    let drain_until = Instant::now() + DRAIN_GRACE;
    while let Some(left) = drain_until.checked_duration_since(Instant::now()) {
        match output.recv_timeout(left) {
            Ok(chunk) => log.push(&chunk),
            Err(_) => break,
        }
    }
    drop(output);
    for reader in readers {
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
    drop(guard);
    Ok(end)
}

fn relay(mut pipe: Box<dyn Read + Send>, sender: &mpsc::SyncSender<Vec<u8>>) {
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if sender.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

fn stop(child: &mut std::process::Child) -> Result<(), OrbitError> {
    crate::supervision::terminate_process_group(
        child,
        crate::supervision::termination_signal(),
        KILL_POLL_INTERVAL,
    )
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
