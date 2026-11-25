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

/// v51: nullable provenance; historical values and older writers stay intact.
/// Never backfill old Mach counters with a fabricated unit or multiplier.
pub(super) fn apply_worker_cpu_accounting(conn: &Connection) -> Result<(), OrbitError> {
    super::introspect::add_column_if_missing(conn, "run_resource_minutes", "cpu_accounting", "TEXT")
}

/// v68: the regime a run minute was sampled under, so profiles can tell
/// samples from before a crew model or Orbit release change. Two nullable
/// columns; rows written before them, or by an older binary, have no regime.
pub(super) fn apply_resource_minute_regime(conn: &Connection) -> Result<(), OrbitError> {
    super::introspect::add_column_if_missing(conn, "run_resource_minutes", "crew_model", "TEXT")?;
    super::introspect::add_column_if_missing(conn, "run_resource_minutes", "orbit_version", "TEXT")
}

/// v69: index each sampled run's first minute as its freshness marker. The
/// sampler starts at minute zero, and retention leaves this marker until a
/// prunable run's other minute rows have been removed.
pub(super) fn apply_run_resource_minutes_freshness_index(
    conn: &Connection,
) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_run_resource_minutes_first_sample
         ON run_resource_minutes(workspace_id, run_id)
         WHERE minute_index = 0;",
    )
    .map_err(sqlite_store_error)
}
