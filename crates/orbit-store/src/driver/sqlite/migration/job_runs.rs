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
