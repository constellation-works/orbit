use super::introspect::add_column_if_missing;
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

pub(super) fn apply_agent_execution_identity(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE agent_executions (
        id INTEGER PRIMARY KEY,
        workspace_id TEXT NOT NULL,
        run_id TEXT NOT NULL,
        activity_id TEXT NOT NULL,
        provider TEXT NOT NULL,
        model TEXT,
        identity_json TEXT NOT NULL,
        started_at TEXT NOT NULL
    );
    CREATE INDEX idx_agent_executions_run ON agent_executions(workspace_id, run_id, id);",
    )
    .map_err(sqlite_store_error)?;
    add_column_if_missing(conn, "invocations", "agent_execution_id", "INTEGER")
}
