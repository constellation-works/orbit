use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// Remember first admissions even when no experiment matched the task.
pub(super) fn apply_experiment_admissions(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS experiment_admissions (workspace_id TEXT NOT NULL, task_id TEXT NOT NULL, PRIMARY KEY (workspace_id, task_id));")
        .map_err(sqlite_store_error)
}

/// v70: reopen admission-only rows. Before ORB-19390, admission recorded a
/// row even while experiments were disabled, and that row then kept the task
/// out of every experiment for good. Those rows cannot be told apart from an
/// enabled admission that matched no population, so each such task is
/// admitted once more; rows beside an assignment or exclusion stay sticky.
/// Every later row comes from an enabled admission and is final.
pub(super) fn apply_reopen_legacy_experiment_admissions(
    conn: &Connection,
) -> Result<(), OrbitError> {
    conn.execute_batch(
        "DELETE FROM experiment_admissions WHERE NOT EXISTS (SELECT 1 FROM experiment_assignments a WHERE a.workspace_id = experiment_admissions.workspace_id AND a.task_id = experiment_admissions.task_id);",
    )
    .map_err(sqlite_store_error)
}
