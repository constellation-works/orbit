//! Deadline-bounded child processes.
//!
//! Callers that must not block forever (publication Git, in particular) spawn
//! through [`run_bounded`]. On Unix the child is a process-group leader, so a
//! deadline can signal the child and the descendants that inherited its group.
//! Other targets have no group primitive — [`super::ancestry::current_process_group`]
//! is `None` there — and only the direct child is killed.
//!
//! Both pipes are drained while the child runs, so a verbose child never stalls
//! on a full pipe. Each turn is bounded, so a pipe that stays readable cannot
//! postpone the deadline or the other stream. [`run_bounded_capped`] keeps only
//! a prefix of each stream.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::output_capture::BoundedOutputCapture;
use crate::OrbitError;

const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// How long a SIGTERM may take before the group is SIGKILL'd.
const TERM_GRACE: Duration = Duration::from_millis(200);
/// After the child is gone, keep pulling pipes only this long, and never past
/// the caller's deadline. Dropping the read end then unblocks a writer the
/// group signal did not reach.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);
/// One pipe may keep the supervisor only this long, and for only this many
/// bytes, before the deadline and the other stream are checked again. A
/// producer that refills the pipe faster than `read` reaches `WouldBlock`
/// would otherwise sit in the drain forever.
#[cfg(unix)]
const DRAIN_TURN: Duration = Duration::from_millis(20);
#[cfg(unix)]
const DRAIN_TURN_BYTES: usize = 64 * 1024;

/// Stdout, stderr, and exit status of a child that finished before the deadline.
#[derive(Debug)]
pub struct CapturedOutput {
    /// Exit status of the direct child.
    pub status: ExitStatus,
    /// What the child wrote to stdout, up to the retention limit.
    pub stdout: Vec<u8>,
    /// What the child wrote to stderr, up to the retention limit.
    pub stderr: Vec<u8>,
}

/// Spawn `command` and wait at most `deadline`.
///
/// Stdin is null and both output streams are captured, matching
/// [`Command::output`](std::process::Command::output). The child is placed in
/// its own process group on Unix. When the deadline elapses the group is
/// signalled (SIGTERM, then SIGKILL) and the leader is reaped; the returned
/// error is [`OrbitError::ProcessTimeout`]. A non-zero exit inside the deadline
/// is success of the wait: the status is in [`CapturedOutput`].
///
/// # Errors
///
/// Returns [`OrbitError::Execution`] when the process cannot be spawned or
/// waited on, and [`OrbitError::ProcessTimeout`] when `deadline` elapses first.
pub fn run_bounded(
    command: &mut Command,
    deadline: Duration,
) -> Result<CapturedOutput, OrbitError> {
    run_bounded_capped(command, deadline, usize::MAX)
}

/// [`run_bounded`] that retains at most `output_limit` bytes of each stream.
///
/// Output past the limit is still read, so the child never blocks on a full
/// pipe, but it is discarded; a truncated stream ends with
/// [`OUTPUT_TRUNCATED_MARKER`](super::output_capture::OUTPUT_TRUNCATED_MARKER).
///
/// # Errors
///
/// Same as [`run_bounded`].
pub fn run_bounded_capped(
    command: &mut Command,
    deadline: Duration,
    output_limit: usize,
) -> Result<CapturedOutput, OrbitError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(command);
    let child = command
        .spawn()
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
    supervise(child, deadline, output_limit)
}

#[cfg(unix)]
fn supervise(
    mut child: Child,
    deadline: Duration,
    output_limit: usize,
) -> Result<CapturedOutput, OrbitError> {
    let leader = child.id();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if let Err(error) = prepare_pipes(stdout.as_ref(), stderr.as_ref()) {
        let _ = terminate(&mut child, leader);
        return Err(OrbitError::Execution(error.to_string()));
    }

    let started = Instant::now();
    let mut out = BoundedOutputCapture::new(output_limit);
    let mut err = BoundedOutputCapture::new(output_limit);
    loop {
        let out_turn = drain_pipe(stdout.as_mut(), &mut out)?;
        let err_turn = drain_pipe(stderr.as_mut(), &mut err)?;
        if let Some(status) = child
            .try_wait()
            .map_err(|error| OrbitError::Execution(error.to_string()))?
        {
            // The leader is reaped. Signal the group so a descendant that
            // inherited it and is still holding a pipe is torn down with it.
            signal_owned_group(leader, kill_signal());
            let drain_end =
                Instant::now() + DRAIN_AFTER_EXIT.min(deadline.saturating_sub(started.elapsed()));
            drain_until(stdout.as_mut(), &mut out, out_turn.eof, drain_end)?;
            drain_until(stderr.as_mut(), &mut err, err_turn.eof, drain_end)?;
            return Ok(CapturedOutput {
                status,
                stdout: out.into_bytes(),
                stderr: err.into_bytes(),
            });
        }
        if started.elapsed() >= deadline {
            // The output is discarded; dropping the pipes on return unblocks
            // any writer the group signal missed.
            terminate(&mut child, leader)?;
            return Err(process_timeout(deadline));
        }
        // A turn that stopped on a budget still has a live producer. Keep
        // draining so that producer does not block on a full pipe; an idle
        // pair of pipes waits out the poll instead of spinning.
        if !out_turn.pending && !err_turn.pending {
            thread::sleep(POLL_INTERVAL);
        }
    }
}

#[cfg(not(unix))]
fn supervise(
    mut child: Child,
    deadline: Duration,
    output_limit: usize,
) -> Result<CapturedOutput, OrbitError> {
    let leader = child.id();
    let stdout = spawn_reader(child.stdout.take(), output_limit);
    let stderr = spawn_reader(child.stderr.take(), output_limit);
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| OrbitError::Execution(error.to_string()))?
        {
            return Ok(CapturedOutput {
                status,
                stdout: finish_reader(stdout)?,
                stderr: finish_reader(stderr)?,
            });
        }
        if started.elapsed() >= deadline {
            terminate(&mut child, leader)?;
            let _ = finish_reader(stdout);
            let _ = finish_reader(stderr);
            return Err(process_timeout(deadline));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(not(unix))]
fn spawn_reader<R>(
    pipe: Option<R>,
    output_limit: usize,
) -> Option<std::sync::mpsc::Receiver<io::Result<Vec<u8>>>>
where
    R: Read + Send + 'static,
{
    let mut pipe = pipe?;
    // One shot: the reader sends exactly the finished buffer.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut buf = BoundedOutputCapture::new(output_limit);
        let mut tmp = [0u8; 8192];
        let result = loop {
            match pipe.read(&mut tmp) {
                Ok(0) => break Ok(buf.into_bytes()),
                Ok(n) => {
                    buf.push(&tmp[..n]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => break Err(error),
            }
        };
        let _ = tx.send(result);
    });
    Some(rx)
}

#[cfg(not(unix))]
fn finish_reader(
    rx: Option<std::sync::mpsc::Receiver<io::Result<Vec<u8>>>>,
) -> Result<Vec<u8>, OrbitError> {
    let Some(rx) = rx else {
        return Ok(Vec::new());
    };
    match rx.recv_timeout(DRAIN_AFTER_EXIT) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(OrbitError::Execution(error.to_string())),
        Err(_) => Err(OrbitError::Execution(
            "process output was still open after the child was reaped".to_string(),
        )),
    }
}

fn process_timeout(deadline: Duration) -> OrbitError {
    OrbitError::ProcessTimeout {
        timeout_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        detail: "deadline exceeded".to_string(),
    }
}

/// Outcome of one bounded read from a nonblocking pipe.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct PipeRead {
    /// The pipe reached EOF.
    eof: bool,
    /// The turn stopped at its byte or time budget while the producer may
    /// still be writing. `false` means EOF or `WouldBlock`: the pipe is idle.
    pending: bool,
}

#[cfg(unix)]
fn drain_pipe(
    pipe: Option<&mut impl Read>,
    buf: &mut BoundedOutputCapture,
) -> Result<PipeRead, OrbitError> {
    match pipe {
        Some(pipe) => {
            read_available(pipe, buf).map_err(|error| OrbitError::Execution(error.to_string()))
        }
        None => Ok(PipeRead {
            eof: true,
            pending: false,
        }),
    }
}

/// Read until EOF or `deadline`. Each turn is bounded, so a producer that
/// keeps the pipe readable cannot postpone `deadline`. At least one read
/// happens before giving up.
#[cfg(unix)]
fn drain_until(
    pipe: Option<&mut impl Read>,
    buf: &mut BoundedOutputCapture,
    mut eof: bool,
    deadline: Instant,
) -> Result<(), OrbitError> {
    let Some(pipe) = pipe else {
        return Ok(());
    };
    while !eof {
        let turn =
            read_available(pipe, buf).map_err(|error| OrbitError::Execution(error.to_string()))?;
        eof = turn.eof;
        if eof || Instant::now() >= deadline {
            break;
        }
        if !turn.pending {
            thread::sleep(POLL_INTERVAL);
        }
    }
    Ok(())
}

/// Read what is buffered, for at most one turn.
///
/// `WouldBlock` and EOF finish the turn with `pending: false`. A producer
/// that never blocks ends the turn at [`DRAIN_TURN_BYTES`] or [`DRAIN_TURN`],
/// whichever comes first, with `pending: true`.
#[cfg(unix)]
fn read_available(pipe: &mut impl Read, buf: &mut BoundedOutputCapture) -> io::Result<PipeRead> {
    let turn_end = Instant::now() + DRAIN_TURN;
    let mut tmp = [0u8; 8192];
    let mut taken = 0usize;
    loop {
        match pipe.read(&mut tmp) {
            Ok(0) => {
                return Ok(PipeRead {
                    eof: true,
                    pending: false,
                });
            }
            Ok(n) => {
                buf.push(&tmp[..n]);
                taken = taken.saturating_add(n);
                if taken >= DRAIN_TURN_BYTES || Instant::now() >= turn_end {
                    return Ok(PipeRead {
                        eof: false,
                        pending: true,
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                // Signals can interrupt every nonblocking read. Check the
                // turn clock here too, or a signal storm could bypass the
                // byte-budget check indefinitely.
                if Instant::now() >= turn_end {
                    return Ok(PipeRead {
                        eof: false,
                        pending: true,
                    });
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(PipeRead {
                    eof: false,
                    pending: false,
                });
            }
            Err(error) => return Err(error),
        }
    }
}

fn terminate(child: &mut Child, leader: u32) -> Result<(), OrbitError> {
    signal_owned_group(leader, term_signal());
    let grace_end = Instant::now() + TERM_GRACE;
    let mut reaped = false;
    while Instant::now() < grace_end {
        if child
            .try_wait()
            .map_err(|error| OrbitError::Execution(error.to_string()))?
            .is_some()
        {
            reaped = true;
            break;
        }
        thread::sleep(POLL_INTERVAL);
    }
    signal_owned_group(leader, kill_signal());
    if !reaped {
        match child
            .try_wait()
            .map_err(|error| OrbitError::Execution(error.to_string()))?
        {
            Some(_) => {}
            None => {
                // The leader may have exited between the poll and this kill.
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn prepare_pipes(
    stdout: Option<&std::process::ChildStdout>,
    stderr: Option<&std::process::ChildStderr>,
) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    if let Some(pipe) = stdout {
        set_nonblocking(pipe.as_raw_fd())?;
    }
    if let Some(pipe) = stderr {
        set_nonblocking(pipe.as_raw_fd())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_pipes(
    _stdout: Option<&std::process::ChildStdout>,
    _stderr: Option<&std::process::ChildStderr>,
) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::unix::io::RawFd) -> io::Result<()> {
    // Safety: `fd` is an open pipe this supervisor owns. The two `fcntl` calls
    // only read and replace that descriptor's status flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn term_signal() -> i32 {
    libc::SIGTERM
}

#[cfg(unix)]
fn kill_signal() -> i32 {
    libc::SIGKILL
}

#[cfg(not(unix))]
fn term_signal() -> i32 {
    15
}

#[cfg(not(unix))]
fn kill_signal() -> i32 {
    9
}

/// Signal `leader`'s process group, never the caller's group.
///
/// The child was spawned with `process_group(0)`, so its pgid equals its pid.
/// After the leader has exited, `killpg` of that pgid still reaches descendants
/// that kept the group. A reused pid that is no longer that group leader is
/// left alone.
#[cfg(unix)]
fn signal_owned_group(leader: u32, signal: i32) {
    let Some(leader) = i32::try_from(leader).ok().filter(|pid| *pid > 1) else {
        return;
    };
    // Safety: `getpgrp`, `getpgid`, and `killpg` only query or signal process
    // groups. They do not dereference caller memory.
    let own = unsafe { libc::getpgrp() };
    if leader == own {
        return;
    }
    let current = unsafe { libc::getpgid(leader) };
    if current > 1 && current != leader {
        return;
    }
    unsafe {
        libc::killpg(leader, signal);
    }
}

#[cfg(not(unix))]
fn signal_owned_group(_leader: u32, _signal: i32) {}
