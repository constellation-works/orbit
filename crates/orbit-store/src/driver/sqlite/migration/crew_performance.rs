use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v58 `crew_performance_snapshot`: one persisted implementer by-type cell
/// matrix per workspace, refreshed hourly so a crew draw never scans.
/// Purely additive: an older binary never reads or writes either table.
pub(super) fn apply_crew_performance_snapshot(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS crew_performance_snapshot (
                workspace_id TEXT NOT NULL PRIMARY KEY,
                computed_at TEXT NOT NULL,
                window_days INTEGER NOT NULL,
                types_hash TEXT NOT NULL,
                types_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS crew_performance_cells (
                workspace_id TEXT NOT NULL,
                crew TEXT NOT NULL,
                model TEXT NOT NULL,
                tier TEXT NOT NULL,
                task_type TEXT NOT NULL,
                n INTEGER NOT NULL,
                raw REAL NOT NULL,
                crew_mean REAL NOT NULL,
                score REAL NOT NULL,
                PRIMARY KEY (workspace_id, crew, model, tier, task_type)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
