//! Recovery authority's append-only ledger and frozen baseline.
//!
//! Each entry owns its shape-defining code. Never edit a shipped body: append
//! a new migration for later shapes. The additive baseline preserves every
//! table a pre-ledger binary creates or reads, including the legacy certificate.

use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::Connection;

use crate::contracts::{ForwardCompatibleOpen, MigrationCompatibility, StateComponent};
use crate::driver::sqlite::host::ledger::{
    LedgeredDatabase, Migration, current_schema_version, evaluate_newer_database,
    run_database_migrations,
};

mod baseline;

const DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::RecoveryAuthority,
    noun: "recovery authority database",
    foreign_keys_off: false,
    stamp: None,
};

/// Stable ordered registry of recovery-authority migrations. Append-only:
/// never renumber or edit a shipped entry.
pub(crate) const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "recovery_authority_baseline",
    compat: MigrationCompatibility::Additive,
    apply: baseline::apply_baseline_v1,
}];

const SUPPORTED_VERSION: u32 = MIGRATIONS[MIGRATIONS.len() - 1].version;

pub(super) fn migrate(
    connection: &Connection,
    path: &Path,
) -> Result<Option<ForwardCompatibleOpen>, OrbitError> {
    run_database_migrations(connection, &DATABASE, MIGRATIONS, Some(path))
}

/// Worker lookups must not migrate: they run without authority write grants.
/// Pre-ledger files remain readable, including their optional binding tables.
pub(super) fn check_read_compatibility(connection: &Connection) -> Result<(), OrbitError> {
    let current = current_schema_version(connection)?;
    if current > SUPPORTED_VERSION {
        evaluate_newer_database(connection, &DATABASE, current, SUPPORTED_VERSION)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
