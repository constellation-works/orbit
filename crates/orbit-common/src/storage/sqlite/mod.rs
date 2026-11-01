//! Shared SQLite connection defaults for every Orbit SQLite store.
//!
//! Historically each store (orbit-store `Store`, its ID allocator and task
//! registry, and orbit-search's `LexicalStore`) hand-rolled its own pragma
//! setup, and the copies drifted (missing `foreign_keys` here, missing
//! `busy_timeout` there). [`apply_default_pragmas`] is the single source of
//! truth: call it on every freshly opened connection, then layer any
//! store-specific overrides (e.g. the task registry's `synchronous=FULL`)
//! on top.

mod connection;
mod errors;
mod hardening;
mod header;
mod wal_lease;

pub mod pragma;

pub use connection::{
    ObservationCurrency, OpenedConnection, PragmaOutcome, apply_default_pragmas,
    filesystem_is_read_only, open_observational, open_private,
};
pub use errors::{is_readonly_or_access_sqlite_error, sqlite_error, sqlite_store_error};
pub use hardening::create_private_dir_all;
pub use wal_lease::{WalFileSetLease, lease_wal_file_set};

/// Default `busy_timeout` applied to every Orbit SQLite connection, in
/// milliseconds. Writers under WAL still serialize; this bounds how long a
/// contending connection spins before surfacing `database is locked`.
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 5_000;
