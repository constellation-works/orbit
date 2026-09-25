use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use rusqlite::{Connection, OpenFlags};

use super::schema::{
    apply_schema, assert_readable_schema, assert_registry_user_version, ensure_compatible_schema,
    registry_user_version,
};
use crate::driver::sqlite::read_pool::{ReadGuard, ReadPool};
use crate::fs::path_safety::normalize_path;

/// Task registry handle: one writer connection behind a mutex plus a
/// read-only connection pool, the same shape as [`crate::Store`]. Under WAL
/// readers on their own connections never queue behind the writer, so a
/// long `replace_task_index` transaction no longer stalls every list/show
/// that shares the registry file (orbit-web, the MCP server).
#[derive(Clone)]
pub struct TaskRegistryStore {
    /// The single writer connection. Every mutating statement and every
    /// transaction serializes here; reads go through [`Self::read`].
    pub(super) conn: Arc<Mutex<Connection>>,
    /// Read pool for writable registries. `None` when the registry was
    /// opened read-only, where reads fall back to the writer connection
    /// (see [`crate::driver::sqlite::read_pool`]).
    readers: Option<Arc<ReadPool>>,
    pub(super) workspaces_dir: PathBuf,
}

impl TaskRegistryStore {
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        let registry_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let workspaces_dir = normalize_path(&registry_dir.join("workspaces"));
        let opened = orbit_common::storage::sqlite::open_private(path)?;
        let mut conn = opened.connection;
        let read_only = opened.read_only;
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
        if !read_only && let Err(error) = conn.pragma_update(None, "synchronous", "FULL") {
            let mapped = OrbitError::Store(format!("failed to set synchronous=FULL: {error}"));
            if mapped.is_readonly_or_access_failure() {
                orbit_common::tracing::warn!(
                    target: "orbit.store.task_registry",
                    path = %path.display(),
                    error = %error,
                    "could not set synchronous=FULL on a read-only task registry; continuing for reads"
                );
            } else {
                return Err(mapped);
            }
        }
        // Setup, migration and recovery all need a write transaction, so a
        // registry opened for observation is either already readable as-is or
        // reported as needing writable storage. Attempting them here is how a
        // read-only mount produced `attempt to write a readonly database`.
        if read_only {
            assert_readable_schema(&conn, path)?;
        } else {
            if registry_user_version(&conn)? < super::REGISTRY_SCHEMA_VERSION {
                apply_schema(&conn)?;
            }
            ensure_compatible_schema(&mut conn, path)?;
            assert_registry_user_version(&conn)?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: (!read_only).then(|| Arc::new(ReadPool::new(path.to_path_buf()))),
            workspaces_dir,
        })
    }

    /// Open an existing registry without creating files, applying schema, or
    /// taking a writer connection. Used by a differing-generation read-only join.
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let registry_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let workspaces_dir = normalize_path(&registry_dir.join("workspaces"));
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        assert_readable_schema(&conn, path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: None,
            workspaces_dir,
        })
    }

    /// Check out a read connection: a pooled query-only reader for writable
    /// registries, or the writer mutex for read-only opens. Never takes the
    /// writer mutex on the pooled path, so reads complete while a write
    /// transaction is open.
    pub(super) fn read(&self) -> Result<ReadGuard<'_>, OrbitError> {
        match &self.readers {
            Some(pool) => {
                let (generation, connection) = pool.checkout()?;
                Ok(ReadGuard::pooled(generation, connection, pool))
            }
            None => {
                let guard = self
                    .conn
                    .lock()
                    .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
                Ok(ReadGuard::Writer(guard))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn reader_pool_for_test(&self) -> Option<&ReadPool> {
        self.readers.as_deref()
    }

    /// Root directory that holds per-workspace canonical bundle trees
    /// (`<global>/tasks/workspaces`). Used by task-migration tooling to locate
    /// and enumerate on-disk bundles for a workspace.
    pub(crate) fn workspaces_dir(&self) -> &Path {
        &self.workspaces_dir
    }
}
