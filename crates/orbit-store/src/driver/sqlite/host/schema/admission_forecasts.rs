use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v48 `admission_forecasts`: one row per drain admission pass that built a
/// forecast, holding the forecast at fixed offsets and the pass's decisions,
/// so it can be checked against `host_resource_minutes`. Points and decisions
/// are JSON arrays; `forecast_at` is fixed-width UTC RFC3339 text, so
/// retention compares it as text. A new table only: an older binary never
/// reads or writes it.
pub(super) fn apply_admission_forecasts(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS admission_forecasts (
                workspace_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                forecast_at TEXT NOT NULL,
                mode TEXT NOT NULL,
                cpu_count INTEGER,
                memory_total_bytes INTEGER,
                points_json TEXT NOT NULL,
                admitted_json TEXT NOT NULL,
                fit_admitted_json TEXT NOT NULL,
                fit_skipped_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_admission_forecasts_at
            ON admission_forecasts(forecast_at);
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v50: pull request decisions beside local pass decisions. Nullable so
/// existing records and older writers retain their local-pass semantics.
pub(super) fn apply_pull_forecast_decisions(conn: &Connection) -> Result<(), OrbitError> {
    super::introspect::add_column_if_missing(
        conn,
        "admission_forecasts",
        "pull_decision_json",
        "TEXT",
    )
}

/// v53: the forecast's own baseline and delivery cohort. Historical rows and
/// older writers retain null attribution metadata; no backfill is inferred.
pub(super) fn apply_forecast_attribution(conn: &Connection) -> Result<(), OrbitError> {
    for (column, kind) in [
        ("baseline_cpu_cores", "REAL"),
        ("baseline_memory_bytes", "INTEGER"),
        ("covered_deliveries_json", "TEXT"),
    ] {
        super::introspect::add_column_if_missing(conn, "admission_forecasts", column, kind)?;
    }
    Ok(())
}
