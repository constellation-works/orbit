use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use rusqlite::Connection;

use super::introspect::add_column_if_missing;

/// v79: additive application authority and non-secret audit provenance.
/// Idempotent, so a replay over a store that already holds either part
/// (a lost ledger, a rewound test fixture) converges instead of failing.
pub(super) fn apply_application_consent(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS application_consents (
            consent_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            consent_json TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            revoked INTEGER NOT NULL DEFAULT 0
        )",
    )
    .map_err(sqlite_store_error)?;
    add_column_if_missing(conn, "audit_events", "application_json", "TEXT")
}
