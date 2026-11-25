//! The host-only recovery authority SQLite database (`authority.db`): its file
//! name, open paths, connection pragmas, and ordered schema ledger.

use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::{Connection, OpenFlags};

pub mod schema;

/// File name of the host-only recovery authority SQLite database.
pub const FILE_NAME: &str = "authority.db";

pub(crate) fn authority_error(action: &str, error: rusqlite::Error) -> OrbitError {
    OrbitError::Execution(format!("{action}: {error}"))
}

/// An opened connection to the isolated `authority.db` SQLite file.
#[derive(Debug)]
pub(crate) struct AuthorityDb {
    connection: Connection,
}

impl AuthorityDb {
    /// Open (creating on first use) and migrate the recovery authority database
    /// at `path`, keeping WAL sidecars persistent for read-only workers.
    pub(crate) fn open(path: &Path) -> Result<Self, OrbitError> {
        let connection = Connection::open(path)
            .map_err(|error| authority_error("open recovery authority database", error))?;

        // This is a connection-local flag, not a schema write. Set it before
        // compatibility evaluation so a query-only open or a refused newer
        // schema cannot delete the sidecars on close and strand sandboxed readers.
        persist_wal_sidecars(&connection)?;
        let forward = schema::migrate(&connection, path)?;
        if forward.is_some_and(|open| !open.writable) {
            connection
                .pragma_update(None, "query_only", true)
                .map_err(|error| authority_error("hold recovery authority read-only", error))?;
        } else {
            // WAL keeps the sidecars inside the protected root rather than beside
            // the shared store, and matches how every other Orbit database runs.
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|error| authority_error("enable WAL on recovery authority", error))?;
        }
        connection
            .pragma_update(
                None,
                "synchronous",
                orbit_common::fs::io::sqlite_synchronous("FULL"),
            )
            .map_err(|error| authority_error("harden recovery authority durability", error))?;

        Ok(Self { connection })
    }

    /// Open an existing `authority.db` read-only without migrating, checking
    /// forward-compatibility rules so sandboxed workers without write grants
    /// can resolve bindings safely.
    pub(crate) fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| authority_error("read worker authority", error))?;
        schema::check_read_compatibility(&connection)?;
        Ok(Self { connection })
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Run `body` in one `BEGIN IMMEDIATE` transaction, so a check and the
    /// write it guards are atomic against every other host connection.
    pub(crate) fn immediate<T>(
        &self,
        action: &str,
        body: impl FnOnce(&Connection) -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| authority_error(action, error))?;
        let outcome = body(&self.connection).and_then(|value| {
            self.connection
                .execute_batch("COMMIT")
                .map_err(|error| authority_error(action, error))
                .map(|()| value)
        });
        if outcome.is_err() && !self.connection.is_autocommit() {
            // The refusal or write failure is what the caller needs; a failed
            // rollback leaves nothing committed either way.
            let _ = self.connection.execute_batch("ROLLBACK");
        }
        outcome
    }
}

/// Keep the `-wal` and `-shm` sidecars when the last connection closes.
///
/// Workers read this database read-only from inside their sandbox, which may
/// read the authority root but never write it. SQLite opens a read-only WAL
/// database without write access only when the shared-memory sidecar already
/// exists, and by default the last closing connection deletes both sidecars.
/// When no host connection was open, a worker's binding lookup then failed
/// with "unable to open database file". Persistent sidecars make the lookup
/// independent of whether the host happens to hold the database open.
fn persist_wal_sidecars(connection: &Connection) -> Result<(), OrbitError> {
    let mut enabled: std::ffi::c_int = 1;
    // SAFETY: the handle is live for the borrow of `connection`, and
    // SQLITE_FCNTL_PERSIST_WAL reads and writes exactly one `int`.
    let code = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&raw mut enabled).cast(),
        )
    };
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(OrbitError::Execution(format!(
            "keep recovery authority WAL sidecars: sqlite result code {code}"
        )));
    }
    Ok(())
}
