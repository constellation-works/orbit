use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v91 `agent_task_facts`: per-task history/comment-derived agent scorecard
/// facts, a derived cache keyed by workspace and task. Purely additive: an
/// older binary never reads or writes the table.
pub(super) fn apply_agent_task_facts(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_task_facts (
            workspace_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            format_version INTEGER NOT NULL,
            source TEXT NOT NULL,
            facts_json TEXT NOT NULL,
            PRIMARY KEY (workspace_id, task_id)
        ) WITHOUT ROWID;",
    )
    .map_err(sqlite_store_error)
}
