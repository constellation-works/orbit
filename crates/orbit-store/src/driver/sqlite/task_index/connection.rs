use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, TryLockError};

use orbit_common::fs::io::sqlite_synchronous;
use orbit_common::storage::sqlite::{
    apply_default_pragmas, is_readonly_or_access_sqlite_error, pragma, sqlite_error,
    sqlite_store_error,
};
use orbit_common::{OrbitError, StorageLayer};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

use super::schema::open::{assert_readable_schema, migrate_writable};
use crate::driver::sqlite::host::read_pool::{ReadGuard, ReadPool};
use crate::fs::path_safety::normalize_path;

/// Root of the per-workspace bundle trees, derived from the canonical
/// registry directory. `workspaces/` is created lazily, so normalizing it
/// directly would keep a symlinked spelling on a fresh root and canonicalize
/// it once the directory exists. The task repair gate keys on this path, so
/// every open of one root must produce the same spelling.
fn workspaces_dir_of(path: &Path) -> PathBuf {
    let registry_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    normalize_path(&registry_dir).join("workspaces")
}

/// Task index database handle: one writer connection behind a mutex plus a
/// read-only connection pool, the same shape as [`crate::Store`]. Under WAL
/// readers on their own connections never queue behind the writer, so a
/// long `replace_task_index` transaction no longer stalls every list/show
/// that shares the registry file (orbit-web, the MCP server).
///
/// This is the only place `tasks/index.sqlite` is opened. Its SQL and row
/// mapping live in `crate::repository::task_index`, which reaches the file
/// through [`Self::read`] and [`Self::write`].
#[derive(Clone)]
pub(crate) struct TaskIndexDb {
    path: PathBuf,
    /// The single writer connection. Every mutating statement and every
    /// transaction serializes here; reads go through [`Self::read`].
    conn: Arc<Mutex<Connection>>,
    /// Read pool for writable registries. `None` when the registry was
    /// opened read-only, where reads fall back to the writer connection
    /// (see [`crate::driver::sqlite::host::read_pool`]).
    readers: Option<Arc<ReadPool>>,
    workspaces_dir: PathBuf,
    /// Opened read-only before the registry adopted its ledger, so tables
    /// the registry baseline creates may be missing.
    pre_ledger: bool,
}

impl TaskIndexDb {
    pub(crate) fn open(path: &Path) -> Result<Self, OrbitError> {
        let opened = orbit_common::storage::sqlite::open_private(path)?;
        let workspaces_dir = workspaces_dir_of(path);
        let conn = opened.connection;
        let mut read_only = opened.read_only;
        if !read_only {
            orbit_common::storage::sqlite::create_private_dir_all(&workspaces_dir)?;
        }
        // The registry is the commit point that makes a created task official, so
        // its writes must be durable against power loss the moment they ack. WAL's
        // synchronous=NORMAL default only fsyncs the WAL at checkpoint, leaving an
        // acked register_task_bundle exposed to rollback on a hard reset. FULL
        // fsyncs the WAL on every commit, closing that window. The registry is
        // low-write (≈one commit per task create/bind/unregister), so the extra
        // fsync cost is negligible. Scoped to this connection only — the shared
        // Store::open stays at NORMAL for higher-write stores.
        if !read_only
            && let Err(error) = pragma::set(
                &conn,
                "synchronous",
                &orbit_common::fs::io::sqlite_synchronous("FULL"),
            )
        {
            if is_readonly_or_access_sqlite_error(&error) {
                orbit_common::tracing::warn!(
                    target: "orbit.store.task_registry",
                    path = %path.display(),
                    error = %error,
                    "could not set synchronous=FULL on a read-only task registry; continuing for reads"
                );
            } else {
                return Err(sqlite_error(
                    StorageLayer::Store,
                    &error,
                    format!("failed to set synchronous=FULL: {error}"),
                ));
            }
        }
        // Adoption and migration need a write transaction, so a registry
        // opened for observation is either already readable as-is or reported
        // as needing writable storage. Attempting them here is how a read-only
        // mount produced `attempt to write a readonly database`.
        let mut pre_ledger = false;
        if read_only {
            pre_ledger = assert_readable_schema(&conn, path)?.pre_ledger;
        } else if migrate_writable(&conn, path, &workspaces_dir)?
            .is_some_and(|forward| !forward.writable)
        {
            // A newer registry this binary may only read: pin the writer as
            // `Store::open` does, and route reads through it.
            pragma::set(&conn, "query_only", &true).map_err(|error| {
                sqlite_error(
                    StorageLayer::Store,
                    &error,
                    format!("failed to hold a newer task registry read-only: {error}"),
                )
            })?;
            read_only = true;
        }

        Ok(Self {
            path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(conn)),
            readers: (!read_only).then(|| Arc::new(ReadPool::new(path.to_path_buf()))),
            workspaces_dir,
            pre_ledger,
        })
    }

    /// Whether this registry was opened for observation only. Answered from
    /// how it was opened, without the writer mutex: every freshness scan
    /// asks, and asking the writer connection queued a task read behind
    /// whichever write held it.
    pub(crate) fn is_read_only(&self) -> Result<bool, OrbitError> {
        Ok(self.readers.is_none())
    }

    /// Open an existing registry without creating files, applying schema, or
    /// taking a writer connection. Used by a differing-generation read-only join.
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let workspaces_dir = workspaces_dir_of(path);
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_store_error)?;
        let readable = assert_readable_schema(&conn, path)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
            workspaces_dir,
            pre_ledger: readable.pre_ledger,
        })
    }

    /// Check out a read connection: a pooled query-only reader for writable
    /// registries, or the writer mutex for read-only opens. Never takes the
    /// writer mutex on the pooled path, so reads complete while a write
    /// transaction is open.
    pub(crate) fn read(&self) -> Result<ReadGuard<'_>, OrbitError> {
        match &self.readers {
            Some(pool) => {
                let (generation, connection) = pool.checkout()?;
                Ok(ReadGuard::pooled(generation, connection, pool))
            }
            None => {
                let guard = orbit_common::fs::lock_wait::mutex(&self.conn)
                    .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
                Ok(ReadGuard::Writer(guard))
            }
        }
    }

    /// Root directory that holds per-workspace canonical bundle trees
    /// (`<global>/tasks/workspaces`). Used by task-migration tooling to locate
    /// and enumerate on-disk bundles for a workspace.
    pub(crate) fn workspaces_dir(&self) -> &Path {
        &self.workspaces_dir
    }

    /// Opened read-only before the registry adopted its ledger, so tables
    /// the registry baseline creates may be missing.
    pub(crate) fn pre_ledger(&self) -> bool {
        self.pre_ledger
    }

    /// Open a dedicated read-only connection for a caller that compares
    /// `PRAGMA data_version` across reads: SQLite versions are comparable
    /// only on the same connection, so a pooled reader cannot serve it.
    pub(crate) fn open_reader(&self) -> Result<Connection, OrbitError> {
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_store_error)?;
        apply_default_pragmas(&connection)?;
        Ok(connection)
    }

    /// Run `write` in one `BEGIN IMMEDIATE` transaction on the writer
    /// connection, committing when it returns `Ok` and rolling back otherwise.
    pub(crate) fn write<T>(
        &self,
        write: impl FnOnce(&Transaction<'_>) -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite_store_error)?;
        let value = write(&tx)?;
        tx.commit().map_err(sqlite_store_error)?;
        Ok(value)
    }

    /// The named second write path: no-wait, non-durable writes for derived
    /// caches, which a reader must never queue behind. `Ok(false)` without
    /// calling `write` when the writer is busy in this process. The writer's
    /// settings are restored before its mutex is released, including on
    /// failure.
    pub(crate) fn cache_write(
        &self,
        write: impl FnOnce(&mut Connection) -> Result<bool, OrbitError>,
    ) -> Result<bool, OrbitError> {
        let mut conn = match self.conn.try_lock() {
            Ok(conn) => conn,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(e)) => {
                return Err(OrbitError::Store(format!("mutex poisoned: {e}")));
            }
        };
        let settings = CacheWriteSettings::apply(&conn)?;
        let recorded = write(&mut conn);
        let restored = settings.restore(&conn);
        let recorded = recorded?;
        restored?;
        Ok(recorded)
    }
}

/// The writer connection's busy timeout and sync level while it records
/// stamps: no wait for the write lock, and in WAL mode no fsync at commit.
/// Both are per-connection, so [`Self::restore`] puts them back before the
/// writer mutex is released: the busy timeout as it was, and the durable
/// level [`TaskIndexDb::open`] gives the writer.
struct CacheWriteSettings {
    busy_timeout_ms: i64,
}

impl CacheWriteSettings {
    fn apply(conn: &Connection) -> Result<Self, OrbitError> {
        let settings = Self {
            busy_timeout_ms: pragma::get::<i64>(conn, "busy_timeout").map_err(|e| {
                sqlite_error(StorageLayer::Store, &e, format!("read busy_timeout: {e}"))
            })?,
        };
        let applied = pragma::set(conn, "busy_timeout", &0)
            .and_then(|()| pragma::set(conn, "synchronous", &sqlite_synchronous("NORMAL")))
            .map_err(|e| {
                sqlite_error(
                    StorageLayer::Store,
                    &e,
                    format!("relax envelope stamp write: {e}"),
                )
            });
        if let Err(error) = applied {
            let _ = settings.restore(conn);
            return Err(error);
        }
        Ok(settings)
    }

    fn restore(&self, conn: &Connection) -> Result<(), OrbitError> {
        pragma::set(conn, "synchronous", &sqlite_synchronous("FULL"))
            .and_then(|()| pragma::set(conn, "busy_timeout", &self.busy_timeout_ms))
            .map_err(|e| {
                sqlite_error(
                    StorageLayer::Store,
                    &e,
                    format!("restore registry writer settings: {e}"),
                )
            })
    }
}
