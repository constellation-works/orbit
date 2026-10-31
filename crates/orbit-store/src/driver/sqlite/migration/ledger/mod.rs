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
//!   [`SUPPORTED_SCHEMA_VERSION`] is decided from the
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
//! registry of its own: the store ([`STORE_DATABASE`], [`MIGRATIONS`]), the
//! task registry (`task_registry::ledger`), and the recovery authority in
//! `orbit-core`. Each file carries its own
//! `schema_meta` ledger rows and `migration.compat` record.

mod compatibility;
mod engine;
mod registry;

pub use compatibility::evaluate_newer_database;
pub(crate) use compatibility::read_compat_record;
pub(crate) use engine::{applied_migrations, run_migrations, run_migrations_at_path};
pub use engine::{current_schema_version, run_database_migrations};
pub use registry::{
    AppliedMigration, LedgerStamp, LedgeredDatabase, Migration, SUPPORTED_SCHEMA_VERSION,
};
pub(crate) use registry::{MIGRATIONS, STORE_DATABASE};
