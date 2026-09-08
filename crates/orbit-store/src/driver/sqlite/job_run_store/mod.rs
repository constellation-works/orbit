//! SQLite job-run storage split into focused backend, query, state, and start modules.
//!
//! `backend` owns `SqliteJobRunStore` and the `JobRunStoreBackend` impl.
//! `queries` contains run/step SQL, row mapping, and filter helpers on `Store`.
//! `state` owns pipeline-state read, bulk-read, write, and immediate RMW.
//! `start` is the Start-event arbiter used by `mark_job_run_running` [ORB-10965].
//! `tests` contains the module unit tests; split it further if it grows past
//! the file-size budget.

mod backend;
mod queries;
mod start;
mod state;

pub use backend::SqliteJobRunStore;

#[cfg(test)]
mod tests;
