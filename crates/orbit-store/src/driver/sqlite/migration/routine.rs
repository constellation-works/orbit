use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

fn ensure_routine_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            -- Host-local routine scheduler state [ORB-10021]. Lives only in
            -- the host-global store database and is never synced between
            -- hosts. Routine definitions are per-user YAML under
            -- `.orbit/routines/`.

            -- Per-routine cursor: first observation baseline + last slot
            -- consumed. A routine never fires for slots before its baseline.
            CREATE TABLE IF NOT EXISTS routine_cursors (
                routine_name TEXT PRIMARY KEY,
                baseline_at TEXT NOT NULL,
                last_slot TEXT,
                updated_at TEXT NOT NULL
            );

            -- One row per fire attempt. The (name, slot, attempt) uniqueness
            -- is the idempotency key that prevents double fires for the same
            -- scheduled slot across overlapping or crashed sweeps.
            CREATE TABLE IF NOT EXISTS routine_fires (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                routine_name TEXT NOT NULL,
                slot TEXT NOT NULL,
                attempt INTEGER NOT NULL DEFAULT 1,
                state TEXT NOT NULL,
                run_id TEXT,
                source_workspace TEXT NOT NULL,
                detail TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(routine_name, slot, attempt)
            );

            CREATE INDEX IF NOT EXISTS idx_routine_fires_name_slot
            ON routine_fires(routine_name, slot DESC, attempt DESC);

            CREATE INDEX IF NOT EXISTS idx_routine_fires_state
            ON routine_fires(state);

            -- Host-local suppressions written by `orbit routine pause`;
            -- durable across reboots, invisible to git.
            CREATE TABLE IF NOT EXISTS routine_pauses (
                routine_name TEXT PRIMARY KEY,
                paused_at TEXT NOT NULL,
                actor TEXT
            );
        "#,
    )
    .map_err(sqlite_store_error)
}

/// v11 `routine_scheduler_schema` migration (ORB-10462): routine tables were
/// added only to the mutable v1 baseline after the ledger shipped. Register
/// the idempotent schema step so existing databases receive the same tables
/// as fresh databases before the baseline is frozen by ADR-0287.
pub(super) fn apply_routine_scheduler_schema(conn: &Connection) -> Result<(), OrbitError> {
    ensure_routine_schema(conn)
}

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
