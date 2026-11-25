use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

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
