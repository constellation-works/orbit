//! Audit-event SQL queries backing the `orbit audit list` CLI.
//!
//! L-0009: callers should reach audit data via `orbit audit list --json` —
//! `<workspace>/.orbit/orbit.db` (and -shm/-wal siblings) is an abandoned
//! leftover from pre-two-root binaries, not a mirror of the canonical global
//! `~/.orbit/orbit.db`. The CLI and runtime always use the global store.
//!
//! `row` hydrates full event rows; `insert` writes them; `queries` reads by id
//! or filter and prunes; `stats` and `aggregates` back the reporting surfaces;
//! `incident` groups failures.

mod aggregates;
pub mod incident;
mod insert;
mod queries;
mod row;
mod stats;

use row::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

#[cfg(test)]
#[cfg(test)]
mod tests;
