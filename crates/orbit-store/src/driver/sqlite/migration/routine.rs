use orbit_common::OrbitError;
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
    .map_err(|e| OrbitError::Store(e.to_string()))
}

/// v11 `routine_scheduler_schema` migration (ORB-10462): routine tables were
/// added only to the mutable v1 baseline after the ledger shipped. Register
/// the idempotent schema step so existing databases receive the same tables
/// as fresh databases before the baseline is frozen by ADR-0287.
pub(super) fn apply_routine_scheduler_schema(conn: &Connection) -> Result<(), OrbitError> {
    ensure_routine_schema(conn)
}
