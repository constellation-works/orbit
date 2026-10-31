//! Recovery authority's append-only ledger and frozen baseline.
//!
//! Each entry owns its shape-defining code. Never edit a shipped body: append
//! a new migration for later shapes. The additive baseline preserves every
//! table a pre-ledger binary creates or reads, including the legacy certificate.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::contracts::{ForwardCompatibleOpen, MigrationCompatibility, StateComponent};
use orbit_store::maintenance::migration::{LedgeredDatabase, Migration, run_database_migrations};
#[cfg(unix)]
use orbit_store::maintenance::migration::{current_schema_version, evaluate_newer_database};
use rusqlite::Connection;

use super::error::authority_error;

const DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::RecoveryAuthority,
    noun: "recovery authority database",
    foreign_keys_off: false,
    stamp: None,
};

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "recovery_authority_baseline",
    compat: MigrationCompatibility::Additive,
    apply: apply_baseline_v1,
}];

#[cfg(unix)]
const SUPPORTED_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

pub(super) fn migrate(
    connection: &Connection,
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_database_migrations(connection, &DATABASE, MIGRATIONS, Some(path))
}

/// Worker lookups must not migrate: they run without authority write grants.
/// Pre-ledger files remain readable, including their optional binding tables.
#[cfg(unix)]
pub(super) fn check_read_compatibility(connection: &Connection) -> Result<(), OrbitError> {
    let current = current_schema_version(connection)?;
    if current > SUPPORTED_VERSION {
        evaluate_newer_database(connection, &DATABASE, current, SUPPORTED_VERSION)?;
    }
    Ok(())
}

/// Frozen v1 copy of the five shipped tables, idempotent over their optional
/// first-use shapes. The engine alone creates the ledger's `schema_meta`.
fn apply_baseline_v1(connection: &Connection) -> Result<(), OrbitError> {
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
