//! WAL file-set leasing: keep a WAL database's sidecars linked.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OpenFlags};

use crate::OrbitError;

use super::DEFAULT_BUSY_TIMEOUT_MS;
use super::errors::{sqlite_operation_error, sqlite_path_error};
use super::pragma;

/// A live SQLite connection that keeps one WAL file set linked.
///
/// Linux runtime sandboxes bind the database, WAL, and shared-memory files by
/// descriptor so a pathname replacement cannot redirect a grant. SQLite may
/// normally unlink those sidecars when its last connection closes. Keeping a
/// connection alive until the sandboxed provider exits prevents the bound
/// descriptors from becoming a coherent but obsolete file set.
pub struct WalFileSetLease {
    path: PathBuf,
    _connection: Mutex<Connection>,
}

impl std::fmt::Debug for WalFileSetLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WalFileSetLease")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Keep an existing WAL database's sidecars linked until this lease is dropped.
///
/// Returns `None` for a missing database or a database that is not in WAL mode.
/// The connection does not hold a transaction, so it does not pin a read
/// snapshot or prevent ordinary checkpoints; it only prevents last-close WAL
/// cleanup from replacing the descriptor-backed sandbox file set.
pub fn lease_wal_file_set(path: &Path) -> Result<Option<WalFileSetLease>, OrbitError> {
    if !path
        .try_exists()
        .map_err(|error| sqlite_path_error("inspect", path, error))?
    {
        return Ok(None);
    }

    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| sqlite_operation_error(Some(path), "open WAL file-set lease", &error))?;
    connection
        .busy_timeout(std::time::Duration::from_millis(u64::from(
            DEFAULT_BUSY_TIMEOUT_MS,
        )))
        .map_err(|error| sqlite_operation_error(Some(path), "set lease busy_timeout", &error))?;
    let journal_mode = pragma::get::<String>(&connection, "journal_mode").map_err(|error| {
        sqlite_operation_error(Some(path), "inspect lease journal mode", &error)
    })?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Ok(None);
    }

    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| {
            sqlite_operation_error(Some(path), "initialize WAL file-set lease", &error)
        })?;

    Ok(Some(WalFileSetLease {
        path: path.to_path_buf(),
        _connection: Mutex::new(connection),
    }))
}
