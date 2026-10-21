use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

/// v55 `executor_census` [ORB-17304]: each executor's latest declared
/// capability per workspace, which pull and owner-local admission order their
/// candidates against.
///
/// One row per workspace and machine, upserted with each admission receipt
/// and by the owner's local drain. Purely additive: an older binary never
/// reads or writes the table, and a newer one reads its absence, or a row it
/// cannot decode, as no census.
pub(super) fn apply_executor_census(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS executor_census (
                workspace_id TEXT NOT NULL,
                machine_id TEXT NOT NULL,
                capability_json TEXT NOT NULL,
                run_id TEXT NOT NULL,
                seen_at TEXT NOT NULL,
                PRIMARY KEY (workspace_id, machine_id)
            );
        "#,
    )
    .map_err(sqlite_store_error)
}
