//! Transactional migration ledger shared by Orbit's SQLite files.
//!
//! Each file owns a strictly ordered registry and stores its applied versions
//! in `schema_meta` under `migration.v<NNNN>`, with the migration name and
//! application timestamp. The same transaction records `migration.compat`
//! so an older binary can decide whether to open a newer database.
//!
//! `BEGIN IMMEDIATE` serializes concurrent openers, which re-read the ledger
//! under the write lock. A database created before the ledger adopts it
//! through the first registry entry, which owns the legacy schema rules.

use rusqlite::Connection;

use crate::OrbitError;
use crate::storage::compatibility::{MigrationCompatibility, StateComponent};

/// One SQLite database file that evolves through its own migration registry.
pub struct LedgeredDatabase {
    /// Which versioned state its compatibility decisions are about.
    pub component: StateComponent,
    /// How diagnostics name the database ("store database").
    pub noun: &'static str,
    /// Run each migration with foreign-key enforcement off. SQLite ignores
    /// `PRAGMA foreign_keys` inside a transaction, so a body that rebuilds a
    /// parent table (create, copy, drop, rename) cannot turn it off itself,
    /// and with enforcement on the drop would cascade into the children.
    pub foreign_keys_off: bool,
    /// Written inside each migration's transaction after its ledger row and
    /// compatibility record, from the registry and the version just applied.
    pub stamp: Option<LedgerStamp>,
}

/// A database-specific marker a [`LedgeredDatabase`] keeps in step with its
/// ledger, such as the task registry's `user_version` fence.
pub type LedgerStamp = fn(&Connection, &[Migration], u32) -> Result<(), OrbitError>;

/// One entry in the migration registry.
pub struct Migration {
    /// Positive schema version, strictly increasing within the registry.
    pub version: u32,
    /// Stable name recorded beside this version in the ledger.
    pub name: &'static str,
    /// What this migration means for a binary that does not have it. See
    /// [`MigrationCompatibility`]; declare `Breaking` when in doubt.
    pub compat: MigrationCompatibility,
    /// Frozen migration body, run inside the engine's transaction.
    pub apply: fn(&Connection) -> Result<(), OrbitError>,
}

/// A migration recorded as applied in the `schema_meta` ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    /// Schema version recorded by the ledger.
    pub version: u32,
    /// Stable name recorded at application.
    pub name: String,
    /// Application timestamp in RFC 3339 format.
    pub applied_at: String,
}

mod compatibility;
mod engine;

pub use compatibility::{evaluate_newer_database, read_compat_record};
pub use engine::{applied_migrations, current_schema_version, run_database_migrations};
