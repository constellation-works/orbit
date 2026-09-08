use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::{apply_default_pragmas, open_private};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::driver::sqlite::migration;
use crate::driver::sqlite::read_pool::{ReadGuard, ReadPool};

thread_local! {
    static FILE_OPEN_PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

fn record_file_open(path: &Path) {
    FILE_OPEN_PATHS.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
}

/// SQLite store handle: one writer connection behind a mutex (WAL permits a
/// single writer) plus a read-only connection pool so reads never queue
/// behind writes. See [`crate::driver::sqlite::read_pool`] for the pool shape.
#[derive(Clone)]
pub struct Store {
    /// The single writer connection. Every mutating statement and every
    /// transaction serializes here; lock scope is one method call, never
    /// held across unrelated I/O.
    pub(crate) conn: Arc<Mutex<Connection>>,
    /// Read pool for file-backed stores. `None` for in-memory stores, whose
    /// reads fall back to the writer connection (a second connection would
    /// see a different empty database).
    readers: Option<Arc<ReadPool>>,
}

pub struct StoreTx<'a> {
    pub(crate) tx: Transaction<'a>,
}

impl StoreTx<'_> {
    /// Borrow the transaction as a raw SQLite connection.
    ///
    /// Feature crates use this narrow escape hatch to keep their SQL and row
    /// codecs inside the owning crate while reusing Store's serialized writer
    /// and transaction boundary. The returned connection remains inside this
    /// transaction; callers must not issue transaction-control statements or
    /// re-enter the parent [`Store`] from the callback.
    pub fn connection(&self) -> &Connection {
        &self.tx
    }
}

impl Store {
    pub(crate) fn schema_meta_value(&self, key: &str) -> Result<Option<String>, OrbitError> {
        let conn = self.read()?;
        match conn.query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(OrbitError::Store(err.to_string())),
        }
    }

    pub(crate) fn set_schema_meta_value(&self, key: &str, value: &str) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            r#"INSERT INTO schema_meta(key, value, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at"#,
            rusqlite::params![key, value, crate::now_string()],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(())
    }
    /// Writer-handle constructions of `path` on this thread via [`Store::open`].
    ///
    /// Read-pool checkouts use `Connection::open` and are not counted. In-memory
    /// stores have no file path and are also excluded.
    pub fn thread_file_open_count_for(path: &Path) -> u64 {
        FILE_OPEN_PATHS.with(|paths| {
            paths
                .borrow()
                .iter()
                .filter(|opened| opened.as_path() == path)
                .count() as u64
        })
    }

    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        record_file_open(path);
        let opened = open_private(path)?;
        let conn = opened.connection;
        let read_only = opened.read_only;

        if let Err(error) = migration::apply_schema(&conn) {
            if read_only && error.is_readonly_or_access_failure() {
                orbit_common::tracing::warn!(
                    target: "orbit.store.sqlite",
                    path = %path.display(),
                    error = %error,
                    "skipped schema migration while opening a store for immutable reads"
                );
            } else {
                return Err(error);
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: (!read_only).then(|| Arc::new(ReadPool::new(path.to_path_buf()))),
        })
    }

    /// Open an existing database for an observational probe, without creating
    /// the database, applying migrations, or changing database pragmas. Use a fresh
    /// connection so a cached runtime handle cannot hide a replaced path.
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
        })
    }

    pub fn open_in_memory() -> Result<Self, OrbitError> {
        let conn = Connection::open_in_memory().map_err(|e| OrbitError::Store(e.to_string()))?;
        apply_default_pragmas(&conn)?;
        migration::apply_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
        })
    }

    pub fn with_transaction<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, OrbitError>,
    {
        self.with_transaction_behavior(TransactionBehavior::Deferred, op)
    }

    pub fn with_transaction_behavior<T, F>(
        &self,
        behavior: TransactionBehavior,
        op: F,
    ) -> Result<T, OrbitError>
    where
        F: FnOnce(&mut StoreTx<'_>) -> Result<T, OrbitError>,
    {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        let tx = conn
            .transaction_with_behavior(behavior)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut store_tx = StoreTx { tx };
        let result = op(&mut store_tx)?;
        store_tx
            .tx
            .commit()
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        Ok(result)
    }

    /// Check out a read-only connection for a SELECT-shaped operation.
    ///
    /// File-backed stores hand out pooled reader connections
    /// (`query_only=ON`), so reads proceed concurrently with the writer.
    /// In-memory stores fall back to locking the writer connection. Never
    /// call this while already holding the writer connection (e.g. inside a
    /// [`Store::with_transaction`] closure): the in-memory fallback would
    /// deadlock, exactly as the old direct `conn.lock()` did.
    pub(crate) fn read(&self) -> Result<ReadGuard<'_>, OrbitError> {
        match &self.readers {
            Some(pool) => Ok(ReadGuard::pooled(pool.checkout()?, pool)),
            None => {
                let guard = self
                    .conn
                    .lock()
                    .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
                Ok(ReadGuard::Writer(guard))
            }
        }
    }

    /// Run a SELECT-shaped callback on Store's pooled read connection.
    ///
    /// File-backed stores provide a `query_only=ON` connection; in-memory
    /// stores use the writer connection because separate in-memory SQLite
    /// connections do not share state. Callers must therefore treat the
    /// connection as read-only on every backend and must not re-enter this
    /// Store from the callback.
    pub fn with_read_connection<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce(&Connection) -> Result<T, OrbitError>,
    {
        let conn = self.read()?;
        op(&conn)
    }

    pub fn connection(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    #[cfg(test)]
    pub(crate) fn reader_pool_for_test(&self) -> Option<&ReadPool> {
        self.readers.as_deref()
    }

    /// Current schema version recorded in the migration ledger (0 when no
    /// versioned migration has run). Foundation for `orbit migrate` (P3.4).
    pub fn schema_version(&self) -> Result<u32, OrbitError> {
        let conn = self.read()?;
        migration::current_schema_version(&conn)
    }

    /// All migrations recorded as applied in the `schema_meta` ledger,
    /// ordered by version ascending.
    pub fn applied_migrations(&self) -> Result<Vec<migration::AppliedMigration>, OrbitError> {
        let conn = self.read()?;
        migration::applied_migrations(&conn)
    }

    /// Run `PRAGMA quick_check` (the fast subset of `integrity_check`).
    /// Returns `Ok(())` when SQLite reports `ok`; otherwise an
    /// [`OrbitError::Store`] listing the reported problems.
    pub fn quick_check(&self) -> Result<(), OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("PRAGMA quick_check")
            .map_err(|e| OrbitError::Store(format!("quick_check: {e}")))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| OrbitError::Store(format!("quick_check: {e}")))?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| OrbitError::Store(format!("quick_check: {e}")))?;
        if rows.len() == 1 && rows[0] == "ok" {
            return Ok(());
        }
        Err(OrbitError::Store(format!(
            "quick_check reported problems: {}",
            rows.join("; ")
        )))
    }

    /// Probe the current database path without creating or migrating it.
    /// A fresh read-write connection detects replaced paths and must acquire
    /// the write lock; cached handles and read-only opens cannot prove readiness.
    pub fn check_path_writable(path: &Path) -> Result<(), OrbitError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        conn.busy_timeout(std::time::Duration::from_secs(1))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        check_connection_writable(&conn)
    }

    /// Prove the database accepts writes without mutating it: acquire the
    /// write lock via `BEGIN IMMEDIATE`, then roll back. Fails when the
    /// database file (or its WAL sidecars) is not writable, or when the
    /// write lock cannot be obtained within the busy timeout.
    pub fn check_writable(&self) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        check_connection_writable(&conn)
    }
}

fn check_connection_writable(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .map_err(|error| OrbitError::Store(format!("write probe failed: {error}")))
}
