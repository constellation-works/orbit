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

/// v75: a counter the store bumps whenever the delivery freshness probe
/// (`MAX`/`COUNT` of `job_runs.finished_at` over the runs that have minute
/// rows) may have changed: a run gains its first minute row or loses its
/// last, or a sampled run's `job_runs` row is inserted, deleted, re-keyed or
/// changes `finished_at`. Minute rows of a run that already has one leave it
/// alone, so a live delivery's per-minute samples do not bump it. Triggers
/// keep it, so older binaries' writes bump it too.
pub(super) fn apply_delivery_runs_stamp(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS delivery_runs_stamp (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            stamp INTEGER NOT NULL
        );
        INSERT OR IGNORE INTO delivery_runs_stamp (id, stamp) VALUES (1, 0);

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_minute_insert
        AFTER INSERT ON run_resource_minutes
        WHEN NOT EXISTS (
            SELECT 1 FROM run_resource_minutes
            WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id
              AND minute_index <> NEW.minute_index
        )
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_minute_delete
        AFTER DELETE ON run_resource_minutes
        WHEN NOT EXISTS (
            SELECT 1 FROM run_resource_minutes
            WHERE workspace_id = OLD.workspace_id AND run_id = OLD.run_id
        )
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_minute_rekey
        AFTER UPDATE OF workspace_id, run_id ON run_resource_minutes
        WHEN OLD.workspace_id IS NOT NEW.workspace_id OR OLD.run_id IS NOT NEW.run_id
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_run_insert
        AFTER INSERT ON job_runs
        WHEN EXISTS (
            SELECT 1 FROM run_resource_minutes
            WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id
        )
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_run_delete
        AFTER DELETE ON job_runs
        WHEN EXISTS (
            SELECT 1 FROM run_resource_minutes
            WHERE workspace_id = OLD.workspace_id AND run_id = OLD.run_id
        )
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;

        CREATE TRIGGER IF NOT EXISTS delivery_runs_stamp_run_update
        AFTER UPDATE OF workspace_id, run_id, finished_at ON job_runs
        WHEN (
            OLD.finished_at IS NOT NEW.finished_at
            OR OLD.workspace_id IS NOT NEW.workspace_id
            OR OLD.run_id IS NOT NEW.run_id
        ) AND (
            EXISTS (
                SELECT 1 FROM run_resource_minutes
                WHERE workspace_id = NEW.workspace_id AND run_id = NEW.run_id
            )
            OR EXISTS (
                SELECT 1 FROM run_resource_minutes
                WHERE workspace_id = OLD.workspace_id AND run_id = OLD.run_id
            )
        )
        BEGIN
            UPDATE delivery_runs_stamp SET stamp = stamp + 1 WHERE id = 1;
        END;
        "#,
    )
    .map_err(sqlite_store_error)
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
