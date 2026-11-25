use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v87 crew memory (slice M1): per-model memory spaces as append-only
/// revisions, the global-memory promotion proposals, and the append-only
/// audit of every write, refusal, reset and copy, all keyed by the owner's
/// `workspace_id`. A space's current content is its newest revision; the
/// triggers keep revisions and events append-only. Timestamps are
/// fixed-width UTC RFC3339 text. New tables only: an older binary never
/// reads or writes them.
pub(super) fn apply_crew_memory(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS crew_memory_revisions (
                workspace_id TEXT NOT NULL,
                memory_key TEXT NOT NULL,
                revision INTEGER NOT NULL,
                files_json TEXT NOT NULL,
                content_digest TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                writer_kind TEXT NOT NULL,
                writer_actor TEXT NOT NULL,
                writer_run_id TEXT,
                writer_crew TEXT,
                reason TEXT,
                created_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, memory_key, revision)
            );
            CREATE TRIGGER IF NOT EXISTS crew_memory_revisions_no_update
                BEFORE UPDATE ON crew_memory_revisions
                BEGIN SELECT RAISE(ABORT, 'crew_memory_revisions is append-only'); END;
            CREATE TRIGGER IF NOT EXISTS crew_memory_revisions_no_delete
                BEFORE DELETE ON crew_memory_revisions
                BEGIN SELECT RAISE(ABORT, 'crew_memory_revisions is append-only'); END;
            CREATE TABLE IF NOT EXISTS crew_memory_proposals (
                workspace_id TEXT NOT NULL,
                id TEXT NOT NULL,
                proposing_key TEXT NOT NULL,
                proposing_run_id TEXT,
                text TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                decided_by TEXT,
                decided_at TEXT,
                decision_reason TEXT,
                PRIMARY KEY (workspace_id, id)
            );
            CREATE INDEX IF NOT EXISTS crew_memory_proposals_status
                ON crew_memory_proposals (workspace_id, status);
            CREATE TABLE IF NOT EXISTS crew_memory_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                memory_key TEXT NOT NULL,
                at TEXT NOT NULL,
                action TEXT NOT NULL,
                actor TEXT NOT NULL,
                revision INTEGER,
                detail_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS crew_memory_events_key
                ON crew_memory_events (workspace_id, memory_key, seq);
            CREATE TRIGGER IF NOT EXISTS crew_memory_events_no_update
                BEFORE UPDATE ON crew_memory_events
                BEGIN SELECT RAISE(ABORT, 'crew_memory_events is append-only'); END;
            CREATE TRIGGER IF NOT EXISTS crew_memory_events_no_delete
                BEFORE DELETE ON crew_memory_events
                BEGIN SELECT RAISE(ABORT, 'crew_memory_events is append-only'); END;
        "#,
    )
    .map_err(sqlite_store_error)
}
