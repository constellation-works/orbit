use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v93 `routine_breakers` migration (ORB-25103): host-local routine circuit
/// breakers and their transition log. New tables only, which an older binary
/// never reads or writes.
pub(super) fn apply_routine_breakers(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            -- One row per routine whose breaker has ever tripped or been
            -- reset on this host. `open` stops the sweep firing the routine
            -- except for one trial fire.
            CREATE TABLE IF NOT EXISTS routine_breakers (
                routine_name TEXT PRIMARY KEY,
                source_workspace TEXT NOT NULL,
                state TEXT NOT NULL,
                error_class TEXT,
                last_run_id TEXT,
                tripped_at TEXT,
                trips INTEGER NOT NULL DEFAULT 0,
                next_trial_at TEXT,
                orbit_build TEXT,
                trial_started_at TEXT,
                trial_reason TEXT,
                bypass_requested_at TEXT,
                floor_at TEXT,
                updated_at TEXT NOT NULL
            );

            -- Append-only record of trips, trials, closes, re-opens and the
            -- operator's resets and bypasses.
            CREATE TABLE IF NOT EXISTS routine_breaker_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                routine_name TEXT NOT NULL,
                event TEXT NOT NULL,
                actor TEXT NOT NULL,
                detail TEXT,
                run_id TEXT,
                at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_routine_breaker_events_name
            ON routine_breaker_events(routine_name, id DESC);
        "#,
    )
    .map_err(sqlite_store_error)
}
