use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists};

/// v60 `landed_diff_stats`: one row per landed delivery with its lines added
/// and deleted, split into code, tests and other. Keyed on workspace, task
/// and landed commit, so recording the same landing twice keeps one row.
/// Purely additive: an older binary never reads or writes the table.
pub(super) fn apply_landed_diff_stats(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS landed_diff_stats (
                workspace_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                landed_commit TEXT NOT NULL,
                base_commit TEXT NOT NULL,
                job_run_id TEXT,
                landed_at TEXT NOT NULL,
                recorded_at TEXT NOT NULL,
                source TEXT NOT NULL,
                crew TEXT,
                model TEXT,
                complexity TEXT,
                task_type TEXT,
                code_added INTEGER NOT NULL,
                code_deleted INTEGER NOT NULL,
                code_files INTEGER NOT NULL,
                tests_added INTEGER NOT NULL,
                tests_deleted INTEGER NOT NULL,
                tests_files INTEGER NOT NULL,
                other_added INTEGER NOT NULL,
                other_deleted INTEGER NOT NULL,
                other_files INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, task_id, landed_commit)
            );
            CREATE INDEX IF NOT EXISTS idx_landed_diff_stats_landed_at
                ON landed_diff_stats (workspace_id, landed_at);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v84 `landed_diff_reviewable_lines`: reviewable added and deleted lines
/// from the task's own commits. Both columns are nullable integers with no
/// default. A row written before this migration, or one whose attribution
/// could not be recovered, leaves them NULL. An older binary never reads or
/// writes the columns, and this migration does not recompute existing rows.
pub(super) fn apply_landed_diff_reviewable_lines(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "landed_diff_stats")? {
        return Ok(());
    }
    add_column_if_missing(conn, "landed_diff_stats", "reviewable_added", "INTEGER")?;
    add_column_if_missing(conn, "landed_diff_stats", "reviewable_deleted", "INTEGER")
}
