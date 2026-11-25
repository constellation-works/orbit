//! The host-global SQLite database (`orbit.db`): its file name, open path,
//! connection pragmas, read pool, migration ledger, and frozen schema steps.

pub(crate) mod connection;
pub(crate) mod ledger;
pub(crate) mod read_pool;
pub mod schema;

/// File name of the host-global SQLite database inside Orbit's global root.
pub const FILE_NAME: &str = "orbit.db";
