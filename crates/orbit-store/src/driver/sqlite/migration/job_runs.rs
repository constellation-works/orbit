use orbit_common::OrbitError;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists, table_has_column};

pub(super) fn apply_flat_crew_model(conn: &Connection) -> Result<(), OrbitError> {
    // ADR-0213: keep the legacy role columns nullable for existing databases;
    // new reads fall back to implementer_model when crew_model is not populated.
    add_column_if_missing(conn, "ALTER TABLE job_runs ADD COLUMN crew_model TEXT")
}

pub(super) fn apply_job_run_archive_stage(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "ALTER TABLE job_runs ADD COLUMN archived_at TEXT")
}

/// v19 `job_runs_created_index`: cover the listing's per-workspace
/// `ORDER BY created_at DESC, run_id ASC` so a bounded page stops scanning
/// and sorting the whole workspace history.
///
/// A legacy database may reach this entry with no `job_runs` table, or with
/// the pre-consolidation one that has no `workspace_id`; the current table
/// is created at open time by `ensure_v2_state_consolidation_schema`, which
/// declares the same index. So this entry indexes the table only when it is
/// already the current shape and otherwise leaves it to open time.
pub(super) fn apply_job_runs_created_index(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")?
        || !table_has_column(conn, "job_runs", "created_at")?
    {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_workspace_created
            ON job_runs(workspace_id, created_at DESC, run_id ASC);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v34 `job_runs_job_created_and_retry_indexes`: cover the two per-workspace
/// reads that had no index of their own.
///
/// - `(workspace_id, job_id, created_at DESC, run_id ASC)` serves every
///   per-job newest-first read (the job history, the keyed-submission window,
///   the catalog's last run per job). The older `(workspace_id, job_id,
///   scheduled_at DESC)` index narrows to the job but cannot order by
///   `created_at`, so each such read sorted every run the job had ever had.
/// - `(workspace_id, retry_source_run_id, created_at, run_id)` serves the
///   retry children read, which orders by `created_at, run_id`, and the
///   retry-lineage walk, which needs only the two-column prefix. Only the
///   automation feature indexed `retry_source_run_id`, and on that two-column
///   index the planner still preferred scanning the workspace in `created_at`
///   order for the children read, so a resume or an incident lookup walked
///   every run to find a handful of children. The automation index stays; it
///   is a prefix of this one and costs nothing to leave.
///
/// Like v19, this indexes only the current `job_runs` shape; a legacy table is
/// left to open time.
pub(super) fn apply_job_runs_job_created_and_retry_indexes(
    conn: &Connection,
) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")?
        || !table_has_column(conn, "job_runs", "job_id")?
        || !table_has_column(conn, "job_runs", "created_at")?
        || !table_has_column(conn, "job_runs", "retry_source_run_id")?
    {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_job_created
            ON job_runs(workspace_id, job_id, created_at DESC, run_id ASC);

            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_retry_created
            ON job_runs(workspace_id, retry_source_run_id, created_at ASC, run_id ASC);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

pub(super) fn apply_execution_provenance(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "job_runs")? {
        return Ok(());
    }
    add_column_if_missing(
        conn,
        "ALTER TABLE job_runs ADD COLUMN executed_on_json TEXT",
    )
}

/// v33 `job_run_id_allocations`: every run id a workspace has ever held, so
/// archiving or deleting a run never frees its id for the next submission in
/// the same minute. Automation keys, audit rows and parent child-dispatch
/// records outlive the run row and keep naming the id.
///
/// The backfill reserves the ids still recorded in `job_runs` and the ones
/// `automation_job_keys` still resolves. A run deleted before this migration
/// and named only by audit or invocation rows cannot be reserved: those rows
/// carry no workspace, and reserving the id in every workspace would guess.
pub(super) fn apply_job_run_id_allocations(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS job_run_id_allocations (
                workspace_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                PRIMARY KEY(workspace_id, run_id)
            ) WITHOUT ROWID;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    if table_has_column(conn, "job_runs", "workspace_id")? {
        conn.execute_batch(
            "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id)
             SELECT workspace_id, run_id FROM job_runs",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    if table_exists(conn, "automation_job_keys")? {
        conn.execute_batch(
            "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id)
             SELECT workspace_id, run_id FROM automation_job_keys",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    Ok(())
}
