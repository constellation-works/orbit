//! Frozen registry ledger v3 body: derived decision projections and owners.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

pub(super) fn apply_decision_snapshots(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE task_decision_snapshots (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            snapshot TEXT NOT NULL,
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE
        );
        CREATE INDEX idx_task_decision_snapshots_workspace
            ON task_decision_snapshots(workspace_id);
        CREATE TABLE task_decision_request_owners (
            request_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            PRIMARY KEY(request_id, task_id),
            FOREIGN KEY(task_id) REFERENCES task_decision_snapshots(task_id) ON DELETE CASCADE
        );
        CREATE INDEX idx_task_decision_request_owners_task
            ON task_decision_request_owners(task_id);",
    )
    .map_err(sqlite_store_error)
}
