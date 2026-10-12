//! The supervision request and its outcome.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::super::stopped_descendants::StoppedDescendant;
use super::{OutputSink, ProcessStopCause};

/// What the supervisor writes to the child's stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stdin {
    /// Write nothing: the child keeps the stdin it was spawned with.
    Null,
    /// Write these bytes to the child's stdin pipe, then close it. The child
    /// must have been spawned with a piped stdin. An empty payload closes
    /// stdin at once.
    Payload(Vec<u8>),
}

/// How the supervisor ends a child it stops: at the deadline, when a sink
/// stops the run, when the stdin write fails, or when it intercepts a signal.
/// Cancellation always kills at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// Signal the group (SIGTERM, or the intercepted signal), wait up to this
    /// long for the child and its group to exit, then kill the group.
    Grace(Duration),
    /// Kill the group at once. For a child whose output is all the caller
    /// wants and that has no cleanup worth protecting.
    KillNow,
}

/// A hook supervision runs on its own thread while the child lives.
pub struct Periodic<'a> {
    /// Time between runs; the first runs one interval after supervision
    /// starts. Must be nonzero.
    pub interval: Duration,
    /// Runs between waits on the child, so never once a wait has seen it exit.
    /// Returning a cause ends the group with the request's `termination`
    /// policy. Finish promptly so deadline and cancellation checks are not
    /// delayed. Return `None` to keep supervising.
    pub run: &'a mut dyn FnMut() -> Option<ProcessStopCause>,
}

/// One supervised run of a [`SupervisedChild`](super::SupervisedChild).
pub struct Supervision<'a> {
    pub stdin: Stdin,
    /// Receives the child's stdout when it was spawned piped.
    pub stdout: Box<dyn OutputSink>,
    /// Receives the child's stderr when it was spawned piped.
    pub stderr: Box<dyn OutputSink>,
    /// Wall-clock limit from the start of supervision; `None` waits forever.
    pub timeout: Option<Duration>,
    /// A deadline the caller can move while the child runs, such as one
    /// extended by credit for time spent queued on a shared resource. When
    /// set it replaces `timeout`. Supervision reads it at the start, again at
    /// most every 100 ms, and afresh before acting on its passing.
    pub extendable_deadline: Option<&'a dyn Fn() -> Instant>,
    pub termination: Termination,
    /// Kills the group at once when set; checked every poll.
    pub cancelled: Option<&'a AtomicBool>,
    /// Intercept SIGINT/SIGTERM while the child lives: forward each to its
    /// group, end it, and re-raise the signal to the previous disposition once
    /// the last supervisor finishes. A child from
    /// [`SupervisedChild::spawn_guarded`](super::SupervisedChild::spawn_guarded) is intercepted from before its spawn
    /// whatever this says.
    pub intercept_signals: bool,
    /// How long pipe workers may run after the child is gone before they are
    /// stopped. Only a holder outside the process group keeps a pipe open that
    /// long: output buffered when a reader stops is still read, later output
    /// is discarded, and stdin bytes nobody read are abandoned.
    pub drain_budget: Duration,
    /// End a descendant that stays stopped for this long, so the child's wait
    /// on it returns (see [`crate::process::stopped_descendants`]). The watch samples
    /// the process table, so callers on the process-identity path keep it
    /// off: `identity` spawns `ps` through the bounded runner itself.
    pub stopped_descendants: Option<Duration>,
    /// Called with each descendant the stopped-descendant watch ended or
    /// failed to end, as it happens. The outcome lists them too.
    pub on_stopped_descendant: Option<&'a dyn Fn(&StoppedDescendant)>,
    /// Called once with the child's pid when supervision starts, before any
    /// pipe setup.
    pub on_spawn: Option<&'a dyn Fn(u32)>,
    /// Run a hook periodically while the child lives.
    pub every: Option<Periodic<'a>>,
}

/// What supervising a child observed.
#[derive(Debug)]
pub struct Outcome {
    /// The child exited zero and no sink or periodic check stopped the run.
    pub success: bool,
    pub exit_code: Option<i32>,
    /// What the stdout sink retained.
    pub stdout: Vec<u8>,
    /// What the stderr sink retained.
    pub stderr: Vec<u8>,
    /// The deadline elapsed and the supervisor ended the child's group.
    pub timed_out: bool,
    /// Every stop the supervisor observed, most significant first: a
    /// termination without an exit code, then periodic and sink stops, then an intercepted
    /// signal.
    pub stop_causes: Vec<ProcessStopCause>,
    /// Descendants the stopped-descendant watch ended.
    pub stopped_descendants: Vec<StoppedDescendant>,
    /// A pipe was still held open after the drain budget, so its worker was
    /// stopped instead of reaching EOF.
    pub drain_stopped: bool,
}

impl Outcome {
    /// The most significant stop cause, if any.
    pub fn stop_cause(&self) -> Option<ProcessStopCause> {
        self.stop_causes.first().copied()
    }
}
