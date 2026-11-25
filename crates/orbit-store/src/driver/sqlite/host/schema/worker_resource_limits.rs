use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v73: one bounded breach snapshot per run, independent of minute profiles.
pub(super) fn apply_worker_resource_limits(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE worker_resource_limits (
            workspace_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            evidence_json TEXT NOT NULL,
            PRIMARY KEY (workspace_id, run_id),
            FOREIGN KEY (workspace_id, run_id) REFERENCES job_runs(workspace_id, run_id)
                ON DELETE CASCADE
        );",
    )
    .map_err(sqlite_store_error)
}
