//! Frozen v1 baseline creating the five shipped recovery authority tables.

use orbit_common::OrbitError;
use rusqlite::Connection;

use super::super::authority_error;

/// Frozen v1 copy of the five shipped tables, idempotent over their optional
/// first-use shapes. The engine alone creates the ledger's `schema_meta`.
pub(super) fn apply_baseline_v1(connection: &Connection) -> Result<(), OrbitError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS rebase_recovery_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS rebase_recovery_attempt (
                     run_id          TEXT NOT NULL,
                     step_id         TEXT NOT NULL,
                     attempt         INTEGER NOT NULL,
                     workspace_path  TEXT NOT NULL,
                     head_sha_before TEXT NOT NULL,
                     target_base_sha TEXT NOT NULL,
                     reserved_at     TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS rebase_recovery_attempt_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     attempt        INTEGER NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS worker_process_binding (
            pid INTEGER NOT NULL, identity TEXT NOT NULL, binding_json TEXT NOT NULL,
            PRIMARY KEY(pid, identity)
        ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS worker_namespace_binding (
            namespace_key TEXT PRIMARY KEY, binding_json TEXT NOT NULL
        ) WITHOUT ROWID;",
        )
        .map_err(|error| authority_error("apply recovery authority baseline", error))
}
