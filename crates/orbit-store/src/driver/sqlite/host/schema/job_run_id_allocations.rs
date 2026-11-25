use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::{table_exists, table_has_column};

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
    .map_err(sqlite_store_error)?;
    if table_has_column(conn, "job_runs", "workspace_id")? {
        conn.execute_batch(
            "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id)
             SELECT workspace_id, run_id FROM job_runs",
        )
        .map_err(sqlite_store_error)?;
    }
    if table_exists(conn, "automation_job_keys")? {
        conn.execute_batch(
            "INSERT OR IGNORE INTO job_run_id_allocations(workspace_id, run_id)
             SELECT workspace_id, run_id FROM automation_job_keys",
        )
        .map_err(sqlite_store_error)?;
    }
    Ok(())
}
