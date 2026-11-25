//! Host-local routine cursor, fire, pause and circuit-breaker records.

use rusqlite::Connection;

use crate::store::{Snapshot, Tx};

mod breaker;
mod cursor;
mod fingerprint;
mod fires;
mod pause;

use crate::contracts::{
    RoutineBreakerEvent, RoutineBreakerRecord, RoutineBreakerState, RoutineCursor,
    RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutinePauseRecord,
    RoutineSlotAttempts,
};

/// Routine records in a host-store snapshot or write transaction.
pub struct RoutineRepository<'a, Access> {
    conn: &'a Connection,
    access: &'a Access,
}

impl<'a, 's> RoutineRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            access: snapshot,
        }
    }
}

impl<'a, 'tx> RoutineRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            access: tx,
        }
    }
}
