//! Store facade: consistent read snapshots (`Store::read`) and IMMEDIATE write
//! transactions (`Store::write`) over the host SQLite driver.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior};

use crate::Store;
use crate::driver::sqlite::host::read_pool::ReadGuard;
use crate::repository::notice::NoticeRepository;

/// One consistent read view of the host store.
///
/// Opens a `BEGIN DEFERRED` transaction on a pooled reader connection (or the
/// in-memory writer connection) so multiple repository queries observe the same
/// snapshot, and rolls it back on drop before returning the connection to the
/// pool.
pub struct Snapshot<'a> {
    conn: ReadGuard<'a>,
}

impl Snapshot<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Read-only access to fleet notice records in this snapshot.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_snapshot(self)
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

/// One `IMMEDIATE` write transaction against the host store.
///
/// Created exclusively by [`Store::write`], which commits when the callback
/// returns `Ok` and rolls back on error or unwind. Writing repository methods
/// are available only on repositories obtained from `&Tx`.
pub struct Tx<'a> {
    conn: &'a Connection,
}

impl Tx<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        self.conn
    }

    /// Read-write access to fleet notice records inside this transaction.
    pub fn notice(&self) -> NoticeRepository<'_, Self> {
        NoticeRepository::for_tx(self)
    }
}

impl Store {
    /// Open one consistent read view (`BEGIN DEFERRED`) over the host store.
    ///
    /// File-backed stores use the read-only connection pool (`query_only=ON`);
    /// in-memory stores borrow the writer connection, so callers must not open
    /// a [`Snapshot`] while already inside [`Store::write`] or
    /// [`Store::with_transaction`].
    pub fn read(&self) -> Result<Snapshot<'_>, OrbitError> {
        let conn = self.read_connection()?;
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(sqlite_store_error)?;
        Ok(Snapshot { conn })
    }

    /// Execute `op` inside one `IMMEDIATE` write transaction.
    ///
    /// Commits when `op` returns `Ok`, rolls back on `Err` or unwind, and
    /// refuses writes on forward-compatible read-only handles. This is the
    /// only place a [`Tx`] is created.
    pub fn write<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(&Tx<'_>) -> Result<T, OrbitError>,
    {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |store_tx| {
            let tx = Tx {
                conn: store_tx.connection(),
            };
            op(&tx)
        })
    }
}
