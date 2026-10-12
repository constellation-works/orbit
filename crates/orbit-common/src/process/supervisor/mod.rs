//! The single child-process supervisor.
//!
//! [`SupervisedChild`] holds a spawned process-group leader through
//! [`OwnedChild`](super::OwnedChild) from spawn until it is reaped, and
//! [`SupervisedChild::supervise`] runs it under one [`Supervision`] request:
//! a stdin payload, one [`Stream`] per output (a sink, or a stream the caller
//! already redirected), an optional caller clock, a deadline with its
//! [`Termination`] policy, a cancellation flag, SIGINT/SIGTERM interception
//! with fan-out to live child groups, a bounded post-exit drain, an opt-in
//! stopped-descendant watch, and caller hooks: one at spawn, one run
//! periodically while the child lives with an optional caller stop cause,
//! and a deadline the caller can extend.
//! The [`Outcome`] carries typed stop metadata, so callers never classify a
//! run by parsing its output.
//!
//! Every setup or wait failure kills the child's process group and reaps the
//! child before the error returns, and every pipe worker finishes before
//! [`SupervisedChild::supervise`] returns: a pipe held open by a process
//! outside the group delays the return by at most the drain budget. A host
//! that cannot create the workers' wakeup socket pair (`EMFILE`) does not fail
//! the run: the workers poll a stop flag instead, within the same bound.

mod drain;
mod echo;
mod request;
pub(crate) mod seam;
#[cfg(unix)]
mod signal_guard;
mod sink;
mod stop_cause;
mod wait;

pub use request::{Outcome, Periodic, Stdin, Stream, Supervision, Termination};
pub use sink::{CaptureSink, OutputSink, RelaySink, SinkVerdict};
pub use stop_cause::{
    INTERRUPTED_SIGNAL_NOTE, OUTPUT_CAPTURE_LIMIT_NOTE, ProcessStopCause, TERMINATION_STATUS_NOTE,
};
pub use wait::SupervisedChild;

#[cfg(test)]
mod tests;
