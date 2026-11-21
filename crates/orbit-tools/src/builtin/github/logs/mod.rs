//! Reading one bounded runner-log excerpt out of a `gh` process.
//!
//! `gh run view --log-failed` is the primary read, and it has a live blind
//! spot: for some runs it exits 0 having written nothing at all, and for a
//! workflow that is still running it can reject every run-scoped log even
//! after one of its jobs has completed unsuccessfully. The same completed
//! job's log API can still serve the full runner log. Neither response is
//! evidence that a run passed or that its logs expired, so this module makes
//! one verified recovery attempt through the job log API.
//!
//! The fallback stays inside the boundaries the primary read already has: the
//! same `gh` process contract, the same streaming collector, the same excerpt
//! and scan caps, and the same redaction. It reads only jobs that a verified
//! `gh run view` payload attributes to the requested run, and — for failed
//! scope — only jobs that run itself reported unsuccessful, so a job that
//! passed can never be returned as failed-step evidence.

mod fallback;
mod read;
mod selection;
mod stream;

pub use read::{RunLogRead, read_run_log, read_run_log_guarded};
pub use selection::{
    LogReadBounds, LogScope, MAX_EVIDENCE_LINES, RunLogRequests, SOURCE_JOB_API_LOG,
    SOURCE_RUN_LOG, log_selection_from_input,
};
