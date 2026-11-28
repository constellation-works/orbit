//! Concrete repository for job runs: `job_runs`, their steps
//! (`job_run_steps`), pipeline states (`job_run_states`), task bindings
//! (`job_run_tasks`), reserved ids (`job_run_id_allocations`) and automation
//! action keys (`automation_job_keys`).
//!
//! Reached only through [`Snapshot::job_run`] (read-only) and [`Tx::job_run`]
//! (read-write inside [`crate::Store::write`]). Every method takes the
//! workspace partition it reads or writes: newly minted run ids are unique
//! across the host's shared store, but historical ids can collide across
//! workspaces. Run lifecycle rules (start authority, terminal transitions,
//! admissions, checkpoint compaction) belong to the calling operation; this
//! repository owns SQL and row mapping only.
//!
//! `list_sql` builds the run-list SQL, `rows` decodes rows, `input_keys`
//! hydrates the subset of a run's input a query asks for, `task_runs` seeks
//! indexed task ownership, `write` holds the run upsert and id allocation and
//! `state` the pipeline-state rows.

mod input_keys;
mod list_sql;
mod rows;
mod runs;
mod state;
mod task_runs;
mod write;

use std::marker::PhantomData;

use rusqlite::Connection;

use crate::store::{Snapshot, Tx};

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
const STEP_RUN_ID_CHUNK: usize = 500;

/// Job ids per latest-run lookup, also below SQLite's bound-parameter cap.
const LATEST_JOB_ID_CHUNK: usize = 500;

/// Concrete job-run repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct JobRunRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> JobRunRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> JobRunRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn job_run_full_step_reads() -> u64 {
    rows::JOB_RUN_FULL_STEP_READS.with(std::cell::Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn job_run_rows_hydrated() -> u64 {
    rows::JOB_RUN_ROWS_HYDRATED.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests;
