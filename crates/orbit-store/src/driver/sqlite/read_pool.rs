//! Hand-rolled read-only connection pool for the WAL-backed [`Store`].
//!
//! Under WAL, readers never block the single writer and vice versa — but
//! only if they use *separate* connections. Routing reads through the
//! writer's `Mutex<Connection>` (the pre-ORB-10004 shape) serialized every
//! read behind every write. This pool hands each read its own connection:
//! checkout pops an idle connection or opens a fresh one (never blocks),
//! check-in retains up to [`MAX_IDLE_READERS`] idle connections and drops
//! the rest. No external pooling dependency (r2d2 etc.) is needed for this
//! shape.
//!
//! Reader connections are pinned read-only via `PRAGMA query_only=ON`, so a
//! misrouted write fails loudly instead of racing the writer connection.
//!
//! In-memory stores have no shareable database file, so [`ReadGuard`] falls
//! back to the writer connection there (same behavior as before the pool).
//!
//! [`Store`]: crate::Store

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::apply_default_pragmas;
use rusqlite::Connection;

/// Maximum idle reader connections retained by the pool. Checkouts beyond
/// this never block — they open extra connections that are simply dropped
/// on check-in once the idle list is full.
const MAX_IDLE_READERS: usize = 4;

pub(crate) struct ReadPool {
    path: PathBuf,
    idle: Mutex<Vec<Connection>>,
    generation: AtomicU64,
}

impl ReadPool {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            idle: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
        }
    }

    /// Pop an idle reader or open a fresh one. Never waits on other readers.
    pub(crate) fn checkout(&self) -> Result<(u64, Connection), OrbitError> {
        let generation = self.generation.load(Ordering::Acquire);
        let idle = self
            .idle
            .lock()
            .map_err(|e| OrbitError::Store(format!("read pool mutex poisoned: {e}")))?
            .pop();
        let connection = match idle {
            Some(conn) => Ok(conn),
            None => self.open_reader(),
        }?;
        Ok((generation, connection))
    }

    /// Drop every idle reader so the next checkout resolves the database path
    /// again. A long-lived process uses this after an external child that was
    /// allowed to open the SQLite files has exited: WAL sidecars may have been
    /// retired and recreated while those cached readers were idle.
    pub(crate) fn clear_idle(&self) -> Result<(), OrbitError> {
        let mut idle = self
            .idle
            .lock()
            .map_err(|e| OrbitError::Store(format!("read pool mutex poisoned: {e}")))?;
        self.generation.fetch_add(1, Ordering::Release);
        idle.clear();
        Ok(())
    }

    /// Return a reader to the idle list, dropping it when the list is full
    /// (or when the pool mutex is poisoned — losing a connection is fine).
    fn checkin(&self, generation: u64, conn: Connection) {
        if let Ok(mut idle) = self.idle.lock()
            && generation == self.generation.load(Ordering::Acquire)
            && idle.len() < MAX_IDLE_READERS
        {
            idle.push(conn);
        }
    }

    #[cfg(test)]
    pub(crate) fn idle_len(&self) -> usize {
        self.idle.lock().map(|idle| idle.len()).unwrap_or(0)
    }

    fn open_reader(&self) -> Result<Connection, OrbitError> {
        let conn = Connection::open(&self.path)
            .map_err(|e| OrbitError::Store(format!("open reader connection: {e}")))?;
        apply_default_pragmas(&conn)?;
        conn.pragma_update(None, "query_only", "ON")
            .map_err(|e| OrbitError::Store(format!("failed to set query_only: {e}")))?;
        Ok(conn)
    }
}

/// RAII handle for a read connection: either a pooled reader (returned to
/// the pool on drop) or, for in-memory stores, the writer connection's
/// mutex guard. Derefs to [`rusqlite::Connection`].
pub(crate) enum ReadGuard<'a> {
    Pooled {
        conn: Option<Connection>,
        pool: &'a ReadPool,
        generation: u64,
    },
    Writer(MutexGuard<'a, Connection>),
}

impl<'a> ReadGuard<'a> {
    pub(crate) fn pooled(generation: u64, conn: Connection, pool: &'a ReadPool) -> Self {
        Self::Pooled {
            conn: Some(conn),
            pool,
            generation,
        }
    }
}

impl Deref for ReadGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        match self {
            ReadGuard::Pooled { conn, .. } => match conn {
                Some(conn) => conn,
                // The Option is only vacated inside Drop, after which no
                // deref can happen.
                None => unreachable!("pooled reader taken before drop"),
            },
            ReadGuard::Writer(guard) => guard,
        }
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let ReadGuard::Pooled {
            conn,
            pool,
            generation,
        } = self
            && let Some(conn) = conn.take()
        {
            pool.checkin(*generation, conn);
        }
    }
}
