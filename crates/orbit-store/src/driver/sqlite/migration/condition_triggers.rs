use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v77: condition-trigger state in its own rows, never inside
/// `automation_consumers`, whose state struct an older binary could rewrite
/// without the condition fields. One generation-fenced state row per
/// condition consumer, and an append-only audit of operator actions on it.
pub(super) fn apply_condition_triggers(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE automation_condition_states (
            consumer TEXT PRIMARY KEY,
            generation INTEGER NOT NULL,
            state_json TEXT NOT NULL
        );
        CREATE TABLE automation_condition_audit (
            audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
            consumer TEXT NOT NULL,
            at TEXT NOT NULL,
            audit_json TEXT NOT NULL
        );
        CREATE INDEX idx_automation_condition_audit_consumer
            ON automation_condition_audit(consumer, audit_id);",
    )
    .map_err(sqlite_store_error)
}
