use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v85 notices: fleet notices held by their home workspace's owner, and the
/// append-only post and close history the post-rate limits count. Each
/// notice keeps its full document in `record_json` beside the columns reads
/// filter on. Timestamps are fixed-width UTC RFC3339 text, so they compare
/// lexically. New tables only: an older binary never reads or writes them.
pub(super) fn apply_notices(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS notices (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL,
                scope TEXT NOT NULL,
                kind TEXT NOT NULL,
                dedupe_key TEXT NOT NULL,
                author_class TEXT NOT NULL,
                author_crew TEXT,
                author_task_id TEXT,
                linked_task_id TEXT,
                close_on_linked_done INTEGER NOT NULL,
                expires_at TEXT NOT NULL,
                opened_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                closed_at TEXT,
                revision INTEGER NOT NULL,
                record_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS notices_workspace_key
                ON notices (workspace_id, dedupe_key);
            CREATE INDEX IF NOT EXISTS notices_scope_closed
                ON notices (scope, closed_at);
            CREATE INDEX IF NOT EXISTS notices_linked_task
                ON notices (linked_task_id);
            CREATE TABLE IF NOT EXISTS notice_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                notice_id TEXT NOT NULL,
                at TEXT NOT NULL,
                action TEXT NOT NULL,
                poster_class TEXT NOT NULL,
                poster_crew TEXT,
                poster_task_id TEXT,
                detail_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS notice_events_notice
                ON notice_events (notice_id, seq);
            CREATE INDEX IF NOT EXISTS notice_events_at
                ON notice_events (at);
        "#,
    )
    .map_err(sqlite_store_error)
}
