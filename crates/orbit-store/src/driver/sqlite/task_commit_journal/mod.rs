//! The durable commit decision behind the task/reservation commit boundary.
//!
//! Task bundles are files, the task index is one SQLite database, and
//! reservations are another. No single technology can publish all three
//! atomically, so the decision itself is made in one place: a journal row in
//! the reservation database, flipped from `prepared` to `committed` in the
//! *same* transaction that inserts the reservation and the dependent
//! coordination rows. Everything before that transaction is undone on
//! failure; everything after it is replayed until it lands.
//!
//! This module owns only the SQL. The protocol, its file-side apply, and the
//! serialization boundary live in `repository::task::coordination`.

use crate::contracts::TaskReservationReserveResult;

mod commit;
mod coordination;
mod journal;

/// Outcome of the one transaction that decides a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalCommitOutcome {
    Committed(Option<TaskReservationReserveResult>),
    /// Reservation files overlap an active reservation; nothing was written.
    Conflicted(TaskReservationReserveResult),
    /// A dependent coordination row already exists under this identity.
    RowExists {
        kind: String,
        row_id: String,
    },
}

#[cfg(test)]
mod tests;
