//! Frozen registry ledger v2 body: index each task's `type`.
//!
//! The column is nullable on purpose, like `complexity`: rows written before
//! this entry stay `NULL` until the defect projection's first read rebuilds
//! them from bundles. A binary without this entry keeps writing the registry
//! (the entry is `Additive`); its upserts leave the column as it was, and its
//! inserts leave it `NULL` for the same repair to fill.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

pub(super) fn apply_index_task_type(conn: &Connection) -> Result<(), OrbitError> {
    if !has_task_type_column(conn)? {
        conn.execute(
            "ALTER TABLE task_bundle_index ADD COLUMN task_type TEXT",
            [],
        )
        .map_err(sqlite_store_error)?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_task_bundle_index_workspace_type_created
            ON task_bundle_index(workspace_id, task_type, created_at)",
        [],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

fn has_task_type_column(conn: &Connection) -> Result<bool, OrbitError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(task_bundle_index)")
        .map_err(sqlite_store_error)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)?;
    Ok(columns.iter().any(|column| column == "task_type"))
}
