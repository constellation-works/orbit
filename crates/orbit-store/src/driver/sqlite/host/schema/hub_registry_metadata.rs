use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v8 `hub_registry_metadata` migration (ORB-10267): one singleton hub-global
/// metadata row carrying the configured hub `machine_id` and a monotonic
/// `registry_revision`. Every snapshot-visible host/alias/retirement,
/// ownership, presence, or execution-profile mutation advances the revision
/// exactly once inside its own transaction; per-workspace execution-profile
/// generation remains a separate concept. The `id = 0` primary-key CHECK is
/// the SQLite singleton guard, and the row is seeded here so every reader sees
/// a revision without a lazy insert path.
pub(super) fn apply_hub_registry_metadata(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS hub_registry_metadata (
                id                INTEGER PRIMARY KEY CHECK (id = 0),
                hub_machine_id    TEXT,
                registry_revision INTEGER NOT NULL DEFAULT 0
                    CHECK (
                        typeof(registry_revision) = 'integer'
                        AND registry_revision >= 0
                        AND registry_revision <= 9223372036854775807
                    ),
                updated_at        TEXT NOT NULL,
                CHECK (hub_machine_id IS NULL OR length(hub_machine_id) > 0)
            );

            INSERT OR IGNORE INTO hub_registry_metadata(
                id, hub_machine_id, registry_revision, updated_at
            ) VALUES (0, NULL, 0, datetime('now'));
        "#,
    )
    .map_err(sqlite_store_error)
}
