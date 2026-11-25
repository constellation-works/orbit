//! Versioned schema-migration ledger for the SQLite store (ORB-10003).
//!
//! Migrations are a stable, ordered registry of `(version, name, apply)`
//! entries. Each applied migration is recorded in the `schema_meta`
//! key/value table under `migration.v<NNNN>` (value = migration name,
//! `updated_at` = applied-at timestamp), so the ledger doubles as the
//! data source for a future `orbit migrate` command (P3.4).
//!
//! Guarantees:
//! - Each migration runs inside a single transaction together with its
//!   ledger insert, so an interrupted migration rolls back instead of
//!   leaving half-applied schema (SQLite ALTER/rename-copy-drop batches
//!   are wrappable in a transaction).
//! - Concurrent openers serialize on `BEGIN IMMEDIATE` and re-read the
//!   ledger inside that transaction, so a waiter neither hits
//!   `SQLITE_BUSY_SNAPSHOT` nor re-applies a version another process
//!   just committed.
//! - A database whose recorded version is newer than
//!   `SUPPORTED_SCHEMA_VERSION` is decided from the
//!   `schema_meta` forward-compatibility record a newer binary leaves
//!   behind (ORB-12434): newer by additive migrations only keeps
//!   serving reads and writes, newer by a read-compatible migration opens
//!   read-only, anything else is refused with [`orbit_common::OrbitError::Migration`]
//!   naming the first breaking migration this binary lacks.
//! - Legacy databases created by the pre-ledger idempotent migrations
//!   adopt the ledger transparently: the v1 baseline is the same
//!   idempotent schema code, so running it on an existing database is a
//!   no-op that then records version 1.
//!
//! The engine is not specific to the store database. Each SQLite file that
//! keeps authority describes itself with a [`LedgeredDatabase`] and owns a
//! registry of its own: the store ([`STORE_DATABASE`], `schema::MIGRATIONS`), the
//! task registry (`task_registry::ledger`), and the recovery authority in
//! `orbit-core`. Each file carries its own
//! `schema_meta` ledger rows and `migration.compat` record.

use orbit_common::OrbitError;
use rusqlite::Connection;

use crate::contracts::{MigrationCompatibility, StateComponent};

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

/// The store database (`orbit.db`) and its `schema::MIGRATIONS`.
pub(crate) const STORE_DATABASE: LedgeredDatabase = LedgeredDatabase {
    component: StateComponent::StoreSchema,
    noun: "store database",
    foreign_keys_off: false,
    stamp: None,
};

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
    pub version: u32,
    pub name: String,
    pub applied_at: String,
}

mod compatibility;
mod engine;

pub use compatibility::evaluate_newer_database;
pub(crate) use compatibility::read_compat_record;
pub(crate) use engine::{applied_migrations, run_migrations, run_migrations_at_path};
pub use engine::{current_schema_version, run_database_migrations};
