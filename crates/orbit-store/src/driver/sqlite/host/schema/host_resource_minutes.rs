use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v45 `host_resource_minutes`: host CPU (cores, from `/proc/stat` ticks) and
/// non-available memory (bytes) per wall-clock minute, in the units of
/// `run_resource_minutes`. Concurrent workers offer the same minute and the
/// first insert wins through the primary key. A new table only: an older
/// binary never reads or writes it.
pub(super) fn apply_host_resource_minutes(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS host_resource_minutes (
                minute_at TEXT PRIMARY KEY NOT NULL,
                cpu_cores REAL NOT NULL,
                cpu_count INTEGER NOT NULL,
                memory_bytes INTEGER NOT NULL,
                memory_total_bytes INTEGER NOT NULL,
                samples INTEGER NOT NULL,
                recorded_by_run_id TEXT
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
