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

use super::OwnedChild;
use super::output_capture::BoundedOutputCapture;
#[cfg(unix)]
use super::pipe::set_nonblocking;
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
/// error is [`OrbitError::ProcessTimeout`]. A direct child that leaves its
/// original group is still killed and reaped after the termination grace;
/// its new group is never signalled. A non-zero exit inside the deadline
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
    run_bounded_capped_typed(command, deadline, output_limit).map_err(OrbitError::from)
}

/// Why a bounded run produced no output.
#[derive(Debug)]
pub enum BoundedRunError {
    /// The OS refused to start the child. The error keeps its
    /// [`io::ErrorKind`], so a caller can retry `ExecutableFileBusy` or tell
    /// "program missing" from "permission denied".
    Spawn(io::Error),
    /// The child started, then waiting for it failed or the deadline elapsed
    /// ([`OrbitError::ProcessTimeout`]).
    Run(OrbitError),
}

impl From<BoundedRunError> for OrbitError {
    fn from(error: BoundedRunError) -> Self {
        match error {
            BoundedRunError::Spawn(error) => OrbitError::Execution(error.to_string()),
            BoundedRunError::Run(error) => error,
        }
    }
}

/// [`run_bounded_capped`] that reports a failed spawn as its [`io::Error`].
///
/// # Errors
///
/// [`BoundedRunError::Spawn`] when the process cannot be started, otherwise
/// [`BoundedRunError::Run`] with the same errors as [`run_bounded`].
pub fn run_bounded_capped_typed(
    command: &mut Command,
    deadline: Duration,
    output_limit: usize,
) -> Result<CapturedOutput, BoundedRunError> {
    run_with_completion(command, deadline, output_limit, false)
}

/// [`run_bounded_capped_typed`] that requires both the child and its captured
/// streams to finish within `deadline`.
///
/// On Unix, a descendant retaining stdout or stderr after the child exits
/// keeps the run pending. At the deadline the entire process group is killed
/// and the pipes are closed, with no capture threads left behind. Other
/// bounded runners tear down descendants immediately after the child exits
/// and accept its status instead.
///
/// # Errors
///
/// Same as [`run_bounded_capped_typed`], including a process timeout when
/// inherited output streams remain open past the deadline.
pub fn run_bounded_capped_complete_typed(
    command: &mut Command,
    deadline: Duration,
    output_limit: usize,
) -> Result<CapturedOutput, BoundedRunError> {
    run_with_completion(command, deadline, output_limit, true)
}

fn run_with_completion(
    command: &mut Command,
    deadline: Duration,
    output_limit: usize,
    require_eof: bool,
) -> Result<CapturedOutput, BoundedRunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(command);
    let child = command.spawn().map_err(BoundedRunError::Spawn)?;
    supervise(child, deadline, output_limit, require_eof).map_err(BoundedRunError::Run)
}

#[cfg(unix)]
fn supervise(
    child: Child,
    deadline: Duration,
    output_limit: usize,
    require_eof: bool,
) -> Result<CapturedOutput, OrbitError> {
    let mut child = OwnedChild::from_spawned(child);
    let mut stdout = child.child_mut().stdout.take();
    let mut stderr = child.child_mut().stderr.take();
    #[cfg(test)]
    inject_supervision_failure(SupervisionFailure::PreparePipes, child.pid())?;
    prepare_pipes(stdout.as_ref(), stderr.as_ref())
        .map_err(|error| OrbitError::Execution(error.to_string()))?;

    let started = Instant::now();
    let mut out = BoundedOutputCapture::new(output_limit);
    let mut err = BoundedOutputCapture::new(output_limit);
    let mut status = None;
    loop {
        #[cfg(test)]
        inject_supervision_failure(SupervisionFailure::PipeRead, child.pid())?;
        let out_turn = drain_pipe(stdout.as_mut(), &mut out)?;
        let err_turn = drain_pipe(stderr.as_mut(), &mut err)?;
        if status.is_none() {
            #[cfg(test)]
            inject_supervision_failure(SupervisionFailure::Wait, child.pid())?;
            status = child
                .child_mut()
                .try_wait()
                .map_err(|error| OrbitError::Execution(error.to_string()))?;
        }
        if let Some(status) = status.filter(|_| !require_eof || (out_turn.eof && err_turn.eof)) {
            // The leader is reaped. Signal the group so a descendant that
            // inherited it and is still holding a pipe is torn down with it.
            let _ = child.signal_group(kill_signal());
            let drain_end =
                Instant::now() + DRAIN_AFTER_EXIT.min(deadline.saturating_sub(started.elapsed()));
            drain_until(stdout.as_mut(), &mut out, out_turn.eof, drain_end)?;
            drain_until(stderr.as_mut(), &mut err, err_turn.eof, drain_end)?;
            child.mark_reaped();
            return Ok(CapturedOutput {
                status,
                stdout: out.into_bytes(),
                stderr: err.into_bytes(),
            });
        }
        if started.elapsed() >= deadline {
            // The output is discarded; dropping the pipes on return unblocks
            // any writer the group signal missed.
            if status.is_some() {
                let _ = child.signal_group(kill_signal());
            } else {
                child
                    .terminate(TERM_GRACE)
                    .map_err(|error| OrbitError::Execution(error.to_string()))?;
            }
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
    child: Child,
    deadline: Duration,
    output_limit: usize,
    _require_eof: bool,
) -> Result<CapturedOutput, OrbitError> {
    let mut child = OwnedChild::from_spawned(child);
    let stdout = spawn_reader(child.child_mut().stdout.take(), output_limit);
    let stderr = spawn_reader(child.child_mut().stderr.take(), output_limit);
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .child_mut()
            .try_wait()
            .map_err(|error| OrbitError::Execution(error.to_string()))?
        {
            child.mark_reaped();
            return Ok(CapturedOutput {
                status,
                stdout: finish_reader(stdout)?,
                stderr: finish_reader(stderr)?,
            });
        }
        if started.elapsed() >= deadline {
            child
                .terminate(TERM_GRACE)
                .map_err(|error| OrbitError::Execution(error.to_string()))?;
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

#[cfg(unix)]
pub(crate) fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn isolate_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn prepare_pipes(
    stdout: Option<&std::process::ChildStdout>,
    stderr: Option<&std::process::ChildStderr>,
) -> io::Result<()> {
    use std::os::unix::io::AsFd;
    if let Some(pipe) = stdout {
        set_nonblocking(pipe.as_fd())?;
    }
    if let Some(pipe) = stderr {
        set_nonblocking(pipe.as_fd())?;
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
fn kill_signal() -> i32 {
    libc::SIGKILL
}

// Per-thread fault injection exercises the public bounded-run boundary without
// exhausting host descriptors or corrupting a process-wide wait disposition.
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SupervisionFailure {
    PreparePipes,
    PipeRead,
    Wait,
}

#[cfg(all(test, unix))]
type FailureInjection = (SupervisionFailure, Box<dyn FnOnce(u32)>);

#[cfg(all(test, unix))]
thread_local! {
    static FAILURE: std::cell::RefCell<Option<FailureInjection>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(all(test, unix))]
pub(super) fn with_supervision_failure<T>(
    point: SupervisionFailure,
    on_failure: impl FnOnce(u32) + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<FailureInjection>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.with(|failure| *failure.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(FAILURE.replace(Some((point, Box::new(on_failure)))));
    let result = run();
    assert!(
        FAILURE.with_borrow(Option::is_none),
        "did not reach {point:?}"
    );
    result
}

#[cfg(all(test, unix))]
fn inject_supervision_failure(point: SupervisionFailure, pid: u32) -> Result<(), OrbitError> {
    let injection = FAILURE.with(|failure| {
        let mut failure = failure.borrow_mut();
        if failure
            .as_ref()
            .is_some_and(|(expected, _)| *expected == point)
        {
            failure.take()
        } else {
            None
        }
    });
    if let Some((_, on_failure)) = injection {
        on_failure(pid);
        return Err(OrbitError::Execution(format!(
            "injected bounded supervision failure: {point:?}"
        )));
    }
    Ok(())
}
