use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v44 `run_resource_minutes`: one row per minute of a delivery run's worker
/// scope, keyed by workspace, run and minute index since the worker started.
///
/// The delivery's work class, complexity and resumed marker are stored on
/// every row as captured once per delivery, so a later tag edit never moves
/// history between profiles. Timestamps are fixed-width UTC RFC3339 text, so
/// retention compares them as text. A new table only: an older binary never
/// reads or writes it.
pub(super) fn apply_run_resource_minutes(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS run_resource_minutes (
                workspace_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                minute_index INTEGER NOT NULL,
                minute_started_at TEXT NOT NULL,
                task_id TEXT NOT NULL,
                work_class TEXT NOT NULL,
                complexity TEXT,
                resumed INTEGER NOT NULL DEFAULT 0,
                delivery_admitted_at TEXT,
                cpu_cores REAL NOT NULL,
                memory_bytes INTEGER NOT NULL,
                samples INTEGER NOT NULL,
                PRIMARY KEY (workspace_id, run_id, minute_index)
            );
            CREATE INDEX IF NOT EXISTS idx_run_resource_minutes_started
            ON run_resource_minutes(minute_started_at);
            CREATE INDEX IF NOT EXISTS idx_run_resource_minutes_task
            ON run_resource_minutes(workspace_id, task_id);
        "#,
    )
    .map_err(sqlite_store_error)
}

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
