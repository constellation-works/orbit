use orbit_common::OrbitError;
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// v13 `workspace_claim_scope` migration (ORB-10709, ADR-0352).
///
/// The exclusive workspace claim reuses `task_reservations` rather than a
/// parallel table, because atomic acquisition, TTL, lazy expiry, audit, and a
/// release escape hatch already live there. `scope` is the dimension that keeps
/// the two apart — worker file reservations arbitrate over paths, the claim
/// arbitrates dispatch authority — and every existing row is a file
/// reservation, which is exactly what the column default says.
///
/// `claim_token` is the holder's bearer token. It is deliberately absent from
/// `select_reservation_columns()`, so no reservation read path can carry it
/// into a response.
pub(super) fn apply_workspace_claim_scope(conn: &Connection) -> Result<(), OrbitError> {
    // A database that recorded a ledger version without ever running the
    // baseline has no reservation table to widen. Create it at its baseline
    // shape first so this migration widens a table that exists rather than
    // wedging the ledger; on every ordinary database this is a no-op.
    ensure_task_reservations_schema(conn)?;

    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN scope TEXT NOT NULL DEFAULT 'files'",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN claim_token TEXT",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_scope_release
            ON task_reservations(workspace_id, scope, released_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

pub(super) fn ensure_task_reservations_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS task_reservations (
                reservation_id TEXT PRIMARY KEY,
                workspace_orbit_dir TEXT NOT NULL,
                task_ids_json TEXT NOT NULL,
                files_json TEXT NOT NULL,
                actor TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                released_at TEXT,
                owner_run_id TEXT,
                owner_metadata_json TEXT,
                release_reason TEXT,
                release_metadata_json TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_expires
            ON task_reservations(workspace_orbit_dir, expires_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_release
            ON task_reservations(workspace_orbit_dir, released_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN workspace_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN owner_run_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN owner_metadata_json TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN release_reason TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN release_metadata_json TEXT",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_owner_release
            ON task_reservations(workspace_orbit_dir, owner_run_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_release
            ON task_reservations(workspace_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_expires
            ON task_reservations(workspace_id, expires_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}
