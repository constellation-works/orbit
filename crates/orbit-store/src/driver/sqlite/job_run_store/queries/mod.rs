//! Run and step SQL, row mapping, and filter helpers on `Store`.
//!
//! `runs` holds the public run and step methods, `write` the run upsert and
//! run-id allocation, `list_sql` the run-list SQL builders and `rows` the row
//! decoding helpers. Sibling modules of `job_run_store` reach only the
//! re-exports below.

mod list_sql;
mod rows;
mod runs;
mod write;

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
pub(super) const STEP_RUN_ID_CHUNK: usize = 500;

/// Job ids per latest-run lookup, also below SQLite's bound-parameter cap.
pub(super) const LATEST_JOB_ID_CHUNK: usize = 500;

pub(super) use list_sql::JOB_RUN_COLUMNS;
pub(super) use rows::{get_job_run_for_workspace_conn, read_steps_for_runs, row_to_job_run};
pub(super) use write::{next_run_id_conn, upsert_job_run_for_workspace_conn};

// Only the admitted unit test in `job_run_store/tests` names this path; the
// store itself calls it from `runs` inside `queries`.
#[cfg(test)]
pub(super) use list_sql::job_run_list_sql;
