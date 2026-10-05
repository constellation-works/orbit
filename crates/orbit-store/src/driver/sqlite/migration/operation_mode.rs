use orbit_common::OrbitError;
use rusqlite::Connection;

/// v26 `remove_operation_mode` migration (ORB-12772): drop the operation-mode
/// grant and recovery-ledger tables. The `feature_schema_meta` rows for the
/// retired `operation` feature stay: that ledger is immutable by design, and
/// nothing reads the feature any more.
pub(super) fn apply_remove_operation_mode(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            DROP TABLE IF EXISTS operation_grants;
            DROP TABLE IF EXISTS operation_recovery;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}
