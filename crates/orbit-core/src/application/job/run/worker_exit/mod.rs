//! The durable record of how a pipeline worker process ended, and the reads
//! stale-run reconciliation makes of it [ORB-17179].
//!
//! The run owns this evidence, so `job/run` sits below `job/pipeline`. This
//! module holds the persisted exit-record format (`pipeline.worker.exited`
//! and the claim row's scope fields) and its readers: the newest recorded
//! exit of a worker, whether it was a host-manager stop, and the clause
//! reconciliation appends when it interrupts a run whose worker is gone.
//! Pipeline's supervisor observes and writes the record, and imports the
//! format from here; see `pipeline/worker/exit.rs` for how a worker's exit is
//! observed, written and amended.
//!
//! Reconciliation that interrupts a run as `process_not_found` (or
//! `token_mismatch`) names the supervisor's exit row in its diagnostic. With
//! no row it says no exit status was observed, and reads what the worker's
//! scope, recorded when the worker claimed the run, still reports: its systemd
//! unit result and counters, or that the scope is gone. A terminating signal
//! that row attributes to the host manager (`systemd`), with no recorded
//! memory or task-limit breach, is a hold (`operator_stopped`) rather than
//! `worker_terminated`.
//!
//! `lifecycle` also preserves entry, application return and reaping in the
//! worker log independently of the store. Reconciliation quotes matching log
//! receipts when the observer or its audit is lost, without treating those
//! diagnostic receipts as settlement or timing authority.

pub(crate) mod lifecycle;
mod record;

pub(in crate::application::job) use record::NO_EXIT_OBSERVED;
pub(crate) use record::{
    ScopeEvidence, SignalSender, WORKER_CLAIM_AUDIT, WORKER_EXIT_AUDIT, WorkerExit,
    reconciled_worker_exit_clause, recorded_host_manager_stop, recorded_worker_exit,
};
