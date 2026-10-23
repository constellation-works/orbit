//! CLI provider supervision on the shared supervisor.
//!
//! [`spawn_with_timeout`] runs a provider child under
//! [`orbit_common::process::supervisor`], which owns the wait loop, the pipe
//! workers, the stdin writer, termination and the drain bound. This module
//! keeps only what is engine-specific:
//!
//! - **Request shape.** [`spawn_for_supervision`] spawns the child (sandboxed
//!   when asked) so a caller can register it, bind the plugin broker or take
//!   the Linux post-run guard first; the Linux mount plan outlives
//!   supervision. The provider prompt goes to stdin, and the run uses
//!   kill-at-once termination with the post-kill group drain wait, a
//!   [`spawn::OUTPUT_DRAIN_BUDGET`] drain bound, and no SIGINT/SIGTERM
//!   interception. Native non-Unix stdin is refused; Windows runs Orbit
//!   through WSL2.
//! - **Hooks.** `on_spawn` receives the PID before any pipe setup.
//!   `on_progress` samples the newest stdout lines on the supervising thread
//!   between waits. A configured stall window shares that check and ends a
//!   tree whose stdout, stderr and CPU time have all stayed flat for the
//!   window. Stopped descendants the supervisor ends are reported one
//!   by one. The deadline is extended by build-budget queue credit read from
//!   `WAIT_DIRECTORY_ENV`, re-read at most every 100 ms, and is kept on the
//!   supervision clock that `with_cli_supervision_clock` replaces in tests.
//! - **Capture** ([`capture`]). Each stream's [`output::TracedCaptureSink`]
//!   feeds a [`capture::RollingOutputCapture`] that keeps going past its
//!   limit: under the limit everything is kept; over it a redacted
//!   complete-line prefix plus a raw complete-line protocol tail are kept and
//!   `truncated` is set.
//! - **Trace events** ([`output`]). Every output line becomes a tracing event
//!   in the job's dispatch context, emitted from the supervisor's pipe worker.
//!
//! After the child exits, times out, or a wait fails, the supervisor kills
//! the process group and waits for it to drain, then gives the pipe workers
//! the drain budget to reach EOF. A helper the agent left in its own session
//! (`setsid`) can keep a pipe open past that; its worker is then stopped:
//! a reader still takes what the pipe already buffers, later output is
//! discarded, and undelivered stdin is abandoned. Every worker is joined
//! before supervision returns, and only then are the captures frozen. A
//! supervision error drops the captures and returns a retryable
//! [`super::spawn::SpawnError`].

mod capture;
mod output;
mod spawn;
mod stall;
#[cfg(feature = "test-support")]
mod test_support;

#[cfg(feature = "test-support")]
pub use test_support::with_cli_supervision_clock;

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
