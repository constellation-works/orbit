use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// Durable notices and an acknowledgement cursor, additive to existing leases.
pub(super) fn apply_lease_notices(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(
        conn,
        "task_reservations",
        "notice_cursor",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS lease_notices (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT,
            workspace_orbit_dir TEXT NOT NULL,
            lease_id TEXT NOT NULL REFERENCES task_reservations(reservation_id),
            kind TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            event_key TEXT NOT NULL,
            UNIQUE(lease_id, event_key)
        );
        CREATE INDEX IF NOT EXISTS lease_notices_workspace_seq ON lease_notices(workspace_id, seq);
        CREATE INDEX IF NOT EXISTS lease_notices_dir_seq ON lease_notices(workspace_orbit_dir, seq);"
    ).map_err(sqlite_store_error)
}
