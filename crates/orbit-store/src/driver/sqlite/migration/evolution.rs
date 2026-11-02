use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use rusqlite::Connection;

pub(super) fn apply_evolution(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS evolution_records (
        id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, signature TEXT NOT NULL,
        state TEXT NOT NULL, record_json TEXT NOT NULL);
        CREATE UNIQUE INDEX IF NOT EXISTS evolution_active_signature ON evolution_records(workspace_id, signature) WHERE state <> 'resolved';
        CREATE TABLE IF NOT EXISTS evolution_costs (
        workspace_id TEXT NOT NULL, source_key TEXT NOT NULL, record_id TEXT,
        event_json TEXT NOT NULL, PRIMARY KEY(workspace_id, source_key));")
        .map_err(sqlite_store_error)
}
