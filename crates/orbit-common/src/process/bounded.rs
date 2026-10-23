//! Deadline-bounded child processes.
//!
//! Callers that must not block forever (publication Git, in particular) spawn
//! through [`run_bounded`]. On Unix the child is a process-group leader, so a
//! deadline can signal the child and the descendants that inherited its group.
//! Other targets have no group primitive — [`super::ancestry::current_process_group`]
//! is `None` there — and only the direct child is killed.
//!
//! The shared supervisor drains both pipes on joined workers, so a verbose
//! child never stalls on a full pipe and a held pipe cannot postpone the
//! deadline. [`run_bounded_capped`] keeps only a prefix of each stream.

use std::io;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use super::output_capture::BoundedOutputCapture;
use super::supervisor::{
    OutputSink, SinkVerdict, Stdin, Stream, SupervisedChild, Supervision, Termination,
};
use crate::OrbitError;

/// How long a SIGTERM may take before the group is SIGKILL'd.
const TERM_GRACE: Duration = Duration::from_millis(200);
/// After the child is gone, keep pulling pipes only this long, and never past
/// the caller's deadline. Dropping the read end then unblocks a writer the
/// group signal did not reach.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);
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
/// Returns [`OrbitError::Execution`] when spawning, waiting or output capture
/// fails, and [`OrbitError::ProcessTimeout`] when `deadline` elapses first.
/// Capture failures return only after group cleanup and all pipe workers join;
/// intentional prefix truncation is still a successful capture.
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
    /// The child started, then waiting or output capture failed, or the deadline
    /// elapsed ([`OrbitError::ProcessTimeout`]).
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
    // Spawn without the signal guard so the OS error kind survives and the
    // process-identity probe neither intercepts signals nor samples itself.
    let child = command.spawn().map_err(BoundedRunError::Spawn)?;
    let outcome = SupervisedChild::adopt(child)
        .supervise(Supervision {
            stdin: Stdin::Null,
            stdout: Stream::Sink(Box::new(PrefixSink(BoundedOutputCapture::new(
                output_limit,
            )))),
            stderr: Stream::Sink(Box::new(PrefixSink(BoundedOutputCapture::new(
                output_limit,
            )))),
            timeout: Some(deadline),
            extendable_deadline: None,
            clock: None,
            termination: Termination::Grace(TERM_GRACE),
            cancelled: None,
            intercept_signals: false,
            drain_budget: DRAIN_AFTER_EXIT,
            cap_drain_at_deadline: true,
            require_eof,
            stopped_descendants: None,
            on_stopped_descendant: None,
            on_spawn: None,
            every: None,
        })
        .map_err(BoundedRunError::Run)?;
    if outcome.timed_out {
        return Err(BoundedRunError::Run(OrbitError::ProcessTimeout {
            timeout_ms: u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            detail: "deadline exceeded".to_string(),
        }));
    }
    let status = outcome.status.ok_or_else(|| {
        BoundedRunError::Run(OrbitError::Execution(
            "process exited without a status".to_string(),
        ))
    })?;
    Ok(CapturedOutput {
        status,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
    })
}

/// Keep a prefix while continuing to read all later output.
struct PrefixSink(BoundedOutputCapture);

impl OutputSink for PrefixSink {
    fn push(&mut self, bytes: &[u8]) -> SinkVerdict {
        self.0.push(bytes);
        SinkVerdict::Continue
    }

    fn finish(self: Box<Self>) -> Vec<u8> {
        self.0.into_bytes()
    }
}

#[cfg(unix)]
pub(crate) fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn isolate_process_group(_command: &mut Command) {}

// Preserve the existing bounded-boundary fault fixtures on the shared loop.
#[cfg(all(test, unix))]
pub(super) use super::supervisor::seam::{SupervisionFailure, with_supervision_failure};
