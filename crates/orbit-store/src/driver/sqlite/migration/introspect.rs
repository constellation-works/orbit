use orbit_common::OrbitError;
use rusqlite::Connection;

/// Add `column` to `table` with `definition` (its type and constraints),
/// unless `PRAGMA table_info` already lists it.
///
/// SQLite appends an added column to the end of every stored record, after
/// all existing columns. Reading a column means walking the overflow-page
/// chain of every large value stored before it, so a column added to a table
/// that holds an unbounded payload (a JSON checkpoint, a transcript) makes
/// every read of the new column pay for the whole payload. Never add a column
/// a listing reads behind such a payload: keep payloads in a 1:1 side table,
/// as `job_run_states` holds run pipeline state (schema v38).
pub(super) fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), OrbitError> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

pub(super) fn table_exists(conn: &Connection, table: &str) -> Result<bool, OrbitError> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(exists > 0)
}

pub(super) fn table_has_column(
    conn: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, OrbitError> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    for name in rows {
        let name = name.map_err(|e| OrbitError::Store(e.to_string()))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn table_has_foreign_key_to(
    conn: &Connection,
    table: &str,
    referenced_table: &str,
) -> Result<bool, OrbitError> {
    let pragma = format!("PRAGMA foreign_key_list({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    for name in rows {
        let name = name.map_err(|e| OrbitError::Store(e.to_string()))?;
        if name == referenced_table {
            return Ok(true);
        }
    }
    Ok(false)
}
