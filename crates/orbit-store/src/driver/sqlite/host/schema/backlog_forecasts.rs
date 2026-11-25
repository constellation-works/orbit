use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v62 `backlog_forecasts`: one row per stored backlog forecast, at most one
/// per workspace per hour, holding the inputs summary, the quantiles and the
/// tasks it covered as JSON, so a later scoring pass can compare it with what
/// those tasks did. `forecast_at` is fixed-width UTC RFC3339 text, so it
/// compares lexically. A new table only: an older binary never reads or
/// writes it.
pub(super) fn apply_backlog_forecasts(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS backlog_forecasts (
                workspace_id TEXT NOT NULL,
                forecast_at TEXT NOT NULL,
                model_version INTEGER NOT NULL,
                inputs_json TEXT NOT NULL,
                result_json TEXT NOT NULL,
                tasks_json TEXT NOT NULL,
                PRIMARY KEY (workspace_id, forecast_at)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
