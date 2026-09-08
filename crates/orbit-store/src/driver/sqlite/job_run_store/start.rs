//! [ORB-10965] The single arbiter of job-run start authority.
//!
//! Deliberately not routed through [`super::backend::SqliteJobRunStore::update_run`]:
//! the decision and the write must share one immediate transaction, and the
//! duplicate cases must write nothing at all rather than rewrite identical
//! values.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::process_start_identity_token;
use orbit_types::workflow::{JobRunStartOutcome, JobRunState, RunEvent};
use rusqlite::TransactionBehavior;

use super::queries::{get_job_run_for_workspace_conn, upsert_job_run_for_workspace_conn};
use crate::Store;

pub(super) fn mark_job_run_running(
    store: &Store,
    workspace_id: &str,
    run_id: &str,
    started_at: DateTime<Utc>,
    pid: u32,
) -> Result<JobRunStartOutcome, OrbitError> {
    let pid_start_time = process_start_identity_token(pid);
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let Some(mut run) = get_job_run_for_workspace_conn(&tx.tx, workspace_id, run_id)? else {
            return Ok(JobRunStartOutcome::NotFound);
        };

        // A run that already left `pending` was started by someone.
        // Duplicate at-least-once delivery from that same owner is a
        // no-op; anyone else has lost the race to the incumbent.
        if run.state == JobRunState::Running || run.state.is_terminal() {
            if run.is_owned_by(pid, pid_start_time.as_deref()) {
                return Ok(JobRunStartOutcome::AlreadyStarted);
            }
            return Err(OrbitError::JobRunStartConflict(format!(
                "run '{}' is already {} under owner pid {} (started at {}); \
                 the start attempt from pid {} has no execution authority",
                run_id,
                run.state,
                run.pid
                    .map_or_else(|| "unknown".to_string(), |owner| owner.to_string()),
                run.started_at
                    .map_or_else(|| "unknown".to_string(), |at| at.to_rfc3339()),
                pid,
            )));
        }

        run.state = run
            .state
            .try_transition(RunEvent::Start)
            .map_err(OrbitError::JobRunStateTransition)?;
        run.started_at = Some(started_at);
        run.pid = Some(pid);
        run.pid_start_time = pid_start_time;
        upsert_job_run_for_workspace_conn(&tx.tx, workspace_id, &run, None)?;
        Ok(JobRunStartOutcome::Started)
    })
}
