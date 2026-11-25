use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// Extend the v2 event-type index with `ts`, so one index serves a read of one
/// event type over a window (the audit summary's policy denials). The old
/// two-column index is a prefix of the new one.
pub(super) fn apply_v2_audit_event_type_ts_index(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_v2_audit_events_ws_event_type_ts
         ON v2_audit_events(workspace_id, event_type, ts);
         DROP INDEX IF EXISTS idx_v2_audit_events_ws_event_type;",
    )
    .map_err(sqlite_store_error)
}
