use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v95 `drain_intents` (standing drain phase 1): one row per workspace
/// holding the operator's drain intent, `on` or `paused`, with who set it,
/// from which surface, why and when. A workspace with no row reads as `off`.
/// `updated_at` is fixed-width UTC RFC3339 text. A new table only: an older
/// binary never reads or writes it.
pub(super) fn apply_drain_intents(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS drain_intents (
                workspace_id TEXT PRIMARY KEY,
                intent TEXT NOT NULL,
                actor TEXT NOT NULL,
                source TEXT NOT NULL,
                reason TEXT,
                updated_at TEXT NOT NULL
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
