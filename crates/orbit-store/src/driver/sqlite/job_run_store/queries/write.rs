//! Run upsert and run-id allocation on a live connection, inside the caller's transaction.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{
    JobRun, PipelineState, RunIdRole, run_id_candidate, run_id_minute_stem,
};

use super::super::state::write_state_json_conn;
use super::rows::optional_json;

pub(in crate::driver::sqlite::job_run_store) fn upsert_job_run_for_workspace_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run: &JobRun,
    pipeline_state: Option<&PipelineState>,
) -> Result<(), OrbitError> {
    let input_json = optional_json(&run.input, "job run input")?;
    let knowledge_metrics_json =
        optional_json(&run.knowledge_metrics, "job run knowledge metrics")?;
    let pipeline_state_json = optional_json(&pipeline_state, "job run pipeline state")?;
    let build_json = optional_json(&run.build, "Orbit build")?;
    let executed_on_json = optional_json(&run.executed_on, "execution location")?;
    conn.execute(
        r#"INSERT INTO job_runs(
            run_id, workspace_id, job_id, attempt, state, scheduled_at,
            started_at, finished_at, duration_ms, created_at, pid, pid_start_time,
            input_json, retry_source_run_id, knowledge_metrics_json, resolved_crew,
            crew_model, executed_on_json, build_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
        ON CONFLICT(workspace_id, run_id) DO UPDATE SET
            job_id = excluded.job_id,
            attempt = excluded.attempt,
            state = excluded.state,
            scheduled_at = excluded.scheduled_at,
            started_at = excluded.started_at,
            finished_at = excluded.finished_at,
            duration_ms = excluded.duration_ms,
            created_at = excluded.created_at,
            pid = excluded.pid,
            pid_start_time = excluded.pid_start_time,
            input_json = excluded.input_json,
            retry_source_run_id = excluded.retry_source_run_id,
            knowledge_metrics_json = excluded.knowledge_metrics_json,
            resolved_crew = excluded.resolved_crew,
            crew_model = excluded.crew_model"#,
        rusqlite::params![
            run.run_id,
            workspace_id,
            run.job_id,
            i64::from(run.attempt),
            run.state.to_string(),
            run.scheduled_at.to_rfc3339(),
            run.started_at.map(|ts| ts.to_rfc3339()),
            run.finished_at.map(|ts| ts.to_rfc3339()),
            run.duration_ms.map(|value| value as i64),
            run.created_at.to_rfc3339(),
            run.pid.map(i64::from),
            run.pid_start_time,
            input_json,
            run.retry_source_run_id,
            knowledge_metrics_json,
            run.resolved_crew,
            run.crew_model,
            executed_on_json,
            build_json,
        ],
    )
    .map_err(sqlite_store_error)?;
    // `None` keeps whatever state the run already has.
    if let Some(state_json) = pipeline_state_json {
        write_state_json_conn(conn, workspace_id, &run.run_id, &state_json)?;
    }
    // Reserve the id for good: deleting the row must not free it for
    // `next_run_id_conn` while other records still name it.
    conn.execute(
        "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id) VALUES (?1, ?2)",
        rusqlite::params![workspace_id, run.run_id],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

/// Sequence ceiling for one role inside one minute stem.
const MAX_RUN_ID_SEQUENCE: u32 = 1023;

/// Allocate the next free run id of `role` for the minute `submitted_at` falls
/// in, across every workspace in the host's shared store. Workspace-scoped
/// allocation lets same-slot routines mint identical worker identities.
/// Existing colliding rows remain readable in their workspace partitions.
///
/// Each role numbers its own sequence, so a run's children never consume the
/// numbers its top-level siblings would take and neither borrows the other's
/// shape [ORB-12111]. Call this inside the same transaction that inserts the
/// run: the probe below is only as good as the write it commits with.
///
/// A candidate is free only if no run holds it now and none ever did. Archive
/// and delete remove the row, but automation keys, audit rows and a parent's
/// child dispatches keep naming the id, so handing it out again would resolve
/// those references to unrelated work.
///
/// Exhausting the sequence is an error rather than a fallback id. Roughly a
/// thousand runs of one role on one host inside one minute is already
/// pathological, and any id returned without a free-slot probe behind it would
/// upsert over the live run already holding it.
pub(in crate::driver::sqlite::job_run_store) fn next_run_id_conn(
    conn: &rusqlite::Connection,
    role: RunIdRole,
    submitted_at: DateTime<Utc>,
) -> Result<String, OrbitError> {
    let stem = run_id_minute_stem(submitted_at);
    for sequence in 1..=MAX_RUN_ID_SEQUENCE {
        let candidate = run_id_candidate(&stem, role, sequence);
        let taken: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM job_runs WHERE run_id = ?1)
                     OR EXISTS(SELECT 1 FROM job_run_id_allocations WHERE run_id = ?1)",
                rusqlite::params![candidate],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        if !taken {
            return Ok(candidate);
        }
    }

    Err(OrbitError::Store(format!(
        "run id sequence exhausted: {MAX_RUN_ID_SEQUENCE} {role} runs already recorded for {stem}"
    )))
}
