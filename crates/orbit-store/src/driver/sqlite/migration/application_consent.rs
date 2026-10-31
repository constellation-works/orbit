use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use rusqlite::Connection;

/// v78: additive application authority and non-secret audit provenance.
pub(super) fn apply_application_consent(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE application_consents (consent_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, consent_json TEXT NOT NULL, fingerprint TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0); ALTER TABLE audit_events ADD COLUMN application_json TEXT;").map_err(sqlite_store_error)
}
