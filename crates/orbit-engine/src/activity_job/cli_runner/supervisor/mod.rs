//! CLI subprocess supervisor.
//!
//! # Stdin ownership / cancellation contract
//!
//! The stdin writer owns its prompt buffer and pipe until it finishes. On
//! Unix it polls a nonblocking pipe for space alongside the same owned cancel
//! signal as output readers. Every write checks cancellation, including when
//! the pipe is continuously writable. Once the child exits, times out, or
//! `wait` fails, the supervisor cancels the writer before killing the process
//! group, then joins it within the output readers' shared cleanup window,
//! abandoning undelivered bytes and closing the pipe on the owning thread. A failed
//! nonblocking setup fails supervision after killing and reaping the child;
//! it never starts a blocking writer. Without a wakeup pair, the writer checks
//! cancellation every [`output::CANCEL_FLAG_POLL_INTERVAL`]. Stdin has no separate
//! delivery or cleanup budget. Native non-Unix stdin supervision
//! is unsupported; Windows runs Orbit through WSL2.
//!
//! # Output drain / truncation contract
//!
//! stdout/stderr readers belong to the supervisor until it returns. After the
//! child exits, times out, or `wait` fails, the supervisor kills the process
//! tree and then:
//!
//! 1. **Bounded drain.** Readers keep consuming readable bytes and emitting
//!    tracing line events until EOF or [`output::OUTPUT_READER_JOIN_TIMEOUT`].
//! 2. **Cancel.** If a writer still holds the pipe (an escaped session), the
//!    supervisor sets each reader's cancel flag and wakes it through an owned
//!    pollable cancel fd. The reader then drains at most the bytes already
//!    queued in the pipe when it observed the cancel (capped at
//!    [`output::POST_CANCEL_DRAIN_LIMIT_BYTES`]), so a writer that keeps producing
//!    cannot extend the drain, and stops capturing and emitting. The
//!    supervisor joins the reader thread before returning. It does not close
//!    another thread's pipe descriptor and does not treat a duplicate close
//!    as cancellation.
//! 3. **Capture finish.** Bytes collected before cancel/EOF are frozen by
//!    [`capture::RollingOutputCapture::finish`]: under the limit they are kept in full;
//!    over the limit a redacted complete-line prefix plus a raw complete-line
//!    protocol tail are kept and `truncated` is set. Writes after cancel are discarded and
//!    must not be logged for the completed invocation.
//! 4. **Wait error.** Readers are finalized the same way; the function then
//!    returns [`super::spawn::SpawnError`] and drops the finished capture because the error
//!    type has no output payload.
//!
//! Unix implements wakeup with `poll` on the reader fd plus a `UnixStream`
//! pair. When the pair cannot be created (for example `EMFILE`), the reader
//! still never blocks in `read`: it polls the pipe with a
//! [`output::CANCEL_FLAG_POLL_INTERVAL`] timeout and rechecks the cancel flag, so
//! finalization stays bounded without a wakeup fd. On Unix the supervisor
//! therefore returns within [`output::OUTPUT_READER_JOIN_TIMEOUT`] of process-tree
//! cleanup plus one poll interval and one bounded drain, whatever an escaped
//! writer does. Non-Unix platforms keep a blocking `Read` and cannot
//! interrupt an escaped holder; that path is not tested here.

mod capture;
mod output;
mod spawn;

#[cfg(all(test, unix))]
mod tests;

pub(super) use capture::{CapturedOutput, OutputProgress};
pub(super) use spawn::{
    ProgressReporter, SpawnTraceContext, SpawnWithTimeoutRequest, StoppedDescendantReporter,
    spawn_for_supervision, spawn_with_timeout,
};

/// Default wall-clock timeout when `AgentLoopSpec::wall_clock_timeout_seconds`
/// is zero. Matches §7.6 guidance: CLI subprocesses must have a mandatory
/// wall-clock guard.
pub(crate) const DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS: u64 = 300;
