//! SQLite job-run storage split into focused backend, query, state, and start modules.
//!
//! `backend` owns `SqliteJobRunStore` and the `JobRunStoreBackend` impl.
//! `queries` contains run/step SQL, row mapping, and filter helpers on `Store`.
//! `task_runs` batches independently bounded recent-run reads per task.
//! `state` owns pipeline-state read, bulk-read, write, and immediate RMW.
//! `start` is the Start-event arbiter used by `mark_job_run_running` [ORB-10965].

mod backend;
pub(crate) mod pull;
mod queries;
mod start;
mod state;
mod task_runs;

pub use backend::SqliteJobRunStore;
