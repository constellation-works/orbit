use orbit_common::OrbitError;
use rusqlite::Connection;

/// v21 `task_commit_journal` (ORB-12528).
///
/// `task_commit_journal` holds one row per intended publication of a task
/// transition together with its reservation and dependent coordination rows.
/// The row is inserted `prepared`, flipped to `committed` inside the same
/// transaction that writes those rows, and settled `applied` once the bundle
/// files carry the transition. Recovery reads exactly the unsettled rows:
/// `prepared` rolls back, `committed` rolls forward.
///
/// `task_coordination_rows` holds the dependent rows themselves. Its primary
/// key is the caller's `(workspace_id, kind, row_id)` identity, so replaying a
/// commit is refused at the database rather than duplicated.
pub(super) fn apply_task_commit_journal(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS task_commit_journal (
                journal_id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                state TEXT NOT NULL,
                intent_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                committed_at TEXT,
                applied_at TEXT,
                reservation_id TEXT,
                CHECK (state IN ('prepared', 'committed', 'applied', 'aborted'))
            );

            CREATE INDEX IF NOT EXISTS idx_task_commit_journal_workspace_state
            ON task_commit_journal(workspace_id, state, created_at);

            CREATE TABLE IF NOT EXISTS task_coordination_rows (
                workspace_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                row_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                journal_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, kind, row_id)
            );

            CREATE INDEX IF NOT EXISTS idx_task_coordination_rows_journal
            ON task_coordination_rows(journal_id);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
