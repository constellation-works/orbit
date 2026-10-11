//! Audit-event v2 SQLite storage split into write, read, query, row-mapping and
//! backend modules.
//!
//! `write` holds the insert path, `read` the list, count and tail reads with
//! their `ReadSnapshot`, `query` the filter SQL and page execution, `rows` the
//! row mapping, and `backend` the `V2AuditStoreBackend` impl. `rendezvous` is
//! the test-only hook that interleaves a write into an open tail read.

mod backend;
mod query;
mod read;
mod rows;
mod write;

#[cfg(test)]
mod rendezvous;

#[cfg(test)]
pub(super) use rendezvous::{clear_tail_snapshot_rendezvous, set_tail_snapshot_rendezvous};
