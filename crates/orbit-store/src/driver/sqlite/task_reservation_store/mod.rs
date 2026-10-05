//! SQLite task reservations and workspace claims.
//!
//! `reservations` holds the `Store` reservation API; `rows` the SQL helpers
//! and row mapping it shares with `workspace_claim`.

mod reservations;
mod rows;
mod workspace_claim;

pub(super) use rows::reserve_files_in_tx;
use rows::{expire_reservations_in_scope, reservation_scope_clause, unique_row_id};

#[cfg(test)]
#[cfg(test)]
mod tests;
