//! SQLite connection setup: private opens, observational reads and pragmas.

use std::fs;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{OrbitError, StorageLayer};

use super::DEFAULT_BUSY_TIMEOUT_MS;
use super::errors::{
    is_readonly_or_access_sqlite_error, sqlite_error_detail, sqlite_operation_error,
    sqlite_path_error,
};
use super::hardening::{
    harden_read_only_sqlite_files, harden_sqlite_files, prepare_private_database_file,
    sidecar_metadata, sqlite_sidecar_paths, validated_sqlite_path,
};
use super::header::database_uses_wal;
use super::pragma;

/// Bytes in a WAL file header. A `-wal` of at most this size carries no
/// frames, so the main database file already holds every committed page.
const WAL_HEADER_BYTES: u64 = 32;

/// A file-backed SQLite connection opened under Orbit's filesystem policy.
pub struct OpenedConnection {
    /// The ready-to-use SQLite connection.
    pub connection: Connection,
    /// Whether the database was opened for observation only, without any
    /// ability to write.
    pub read_only: bool,
    /// Whether this connection keeps up with commits made after it was opened.
    /// Writable connections are always [`ObservationCurrency::Live`].
    pub currency: ObservationCurrency,
}

/// Open an Orbit SQLite database without exposing its persisted state.
///
/// Writable databases are created or repaired to owner-only access on Unix.
/// The database is hardened before SQLite can create WAL/SHM sidecars, and
/// pre-existing sidecars are repaired as part of the same operation; a
/// symlinked or otherwise irregular sidecar fails the open without its target
/// being touched. Newly created parent directories are owner-only as well.
/// An existing read-only database on writable storage has group/other
/// permissions removed before it is opened for observation. A database on a
/// read-only filesystem is opened observationally before any directory
/// creation or permission change is attempted; see [`open_observational`] for
/// how such a database is read.
pub fn open_private(path: &Path) -> Result<OpenedConnection, OrbitError> {
    let path = validated_sqlite_path(path)?;

    match fs::metadata(&path) {
        Ok(metadata) => {
            let filesystem_read_only = filesystem_is_read_only(&path)?;
            if filesystem_read_only || metadata.permissions().readonly() {
                return open_private_read_only(&path, filesystem_read_only);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(sqlite_path_error("inspect", &path, error)),
    }

    // Refuse redirected companions before attempting any permission repair.
    for sidecar in sqlite_sidecar_paths(&path) {
        sidecar_metadata(&sidecar)?;
    }
    prepare_private_database_file(&path)?;
    // Sidecars left beside a newly created database are checked too, before
    // SQLite opens them by path.
    harden_sqlite_files(&path)?;

    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| sqlite_operation_error(Some(&path), "cannot open SQLite database", &error))?;
    let pragmas = apply_default_pragmas_for_path(&connection, &path)?;
    if pragmas.write_denied || filesystem_is_read_only(&path)? {
        drop(connection);
        return open_observational(&path);
    }
    harden_sqlite_files(&path)?;

    Ok(OpenedConnection {
        connection,
        read_only: false,
        currency: ObservationCurrency::Live,
    })
}

/// Observe a database Orbit may not write.
///
/// Orbit's stores accept [`ObservationCurrency::MainFileOnly`] deliberately:
/// state published as a static file set — a read-only mount of a directory
/// whose last writer closed cleanly, and so left no wal-index behind — carries
/// no sidecars at all, and refusing it would make such a deployment unreadable
/// rather than stale. Every caller is handed the currency it got, and
/// [`open_observational`] warns when the degraded read is in use.
fn open_private_read_only(
    path: &Path,
    filesystem_read_only: bool,
) -> Result<OpenedConnection, OrbitError> {
    let path = validated_sqlite_path(path)?;

    if !filesystem_read_only {
        harden_read_only_sqlite_files(&path)?;
    }
    open_observational(&path)
}

/// Result of [`apply_default_pragmas`]: what SQLite actually settled on for
/// the best-effort settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaOutcome {
    /// Journal mode active after the WAL request, lowercased by SQLite
    /// convention (`wal`, `memory` for in-memory databases, or a fallback
    /// such as `delete` when the filesystem refuses WAL sidecars).
    pub journal_mode: String,
    /// The connection refused a persistence pragma because its backing store
    /// is read-only. Callers that need sidecar-free reads should reopen it
    /// with [`open_observational`].
    pub write_denied: bool,
}

/// Apply the Orbit-wide SQLite connection defaults:
///
/// - `journal_mode=WAL` — best-effort: when the database file is read-only
///   or the filesystem refuses WAL sidecar writes, we warn and keep the
///   active journal mode so reads still succeed (in-memory databases keep
///   their `memory` mode silently — WAL does not apply to them);
/// - `busy_timeout` = [`DEFAULT_BUSY_TIMEOUT_MS`];
/// - `foreign_keys=ON`;
/// - `synchronous=NORMAL` — the recommended WAL durability level. Stores
///   that need commit-durable acks (e.g. the task registry) override to
///   `FULL` after calling this. Both go through
///   [`sqlite_synchronous`](crate::fs::io::sqlite_synchronous), which is
///   `OFF` in a relaxed test run.
pub fn apply_default_pragmas(conn: &Connection) -> Result<PragmaOutcome, OrbitError> {
    apply_default_pragmas_inner(conn, None)
}

pub(super) fn apply_default_pragmas_for_path(
    conn: &Connection,
    path: &Path,
) -> Result<PragmaOutcome, OrbitError> {
    apply_default_pragmas_inner(conn, Some(path))
}

fn apply_default_pragmas_inner(
    conn: &Connection,
    path: Option<&Path>,
) -> Result<PragmaOutcome, OrbitError> {
    let (journal_mode, mut write_denied) = request_wal_journal_mode(conn, path);
    pragma::set(conn, "busy_timeout", &DEFAULT_BUSY_TIMEOUT_MS)
        .map_err(|error| sqlite_operation_error(path, "set busy_timeout", &error))?;
    pragma::set(conn, "foreign_keys", &"ON")
        .map_err(|error| sqlite_operation_error(path, "enable foreign keys", &error))?;
    if let Err(error) = pragma::set(
        conn,
        "synchronous",
        &crate::fs::io::sqlite_synchronous("NORMAL"),
    ) {
        let mapped = sqlite_operation_error(path, "set synchronous=NORMAL", &error);
        if mapped.is_readonly_or_access_failure() {
            write_denied = true;
            tracing::warn!(
                target: "orbit.common.sqlite",
                error = %error,
                "could not set synchronous=NORMAL on a read-only database; continuing for reads"
            );
        } else {
            return Err(mapped);
        }
    }
    Ok(PragmaOutcome {
        journal_mode,
        write_denied,
    })
}

/// Whether a connection keeps up with commits another process makes after the
/// connection was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationCurrency {
    /// Ordinary access: every statement re-reads the database files, so a
    /// commit that lands after this connection was opened becomes visible to
    /// the next query.
    Live,
    /// `immutable=1`: the connection reads the main database file alone and is
    /// free to cache it forever.
    ///
    /// It is not a guaranteed snapshot. It ignores every `-wal`, including one
    /// a writer creates after the open, so later commits stay invisible; and
    /// because `immutable=1` promises SQLite that the bytes never change, a
    /// writer that later checkpoints into the main file can leave this
    /// connection returning results that are not merely stale but internally
    /// inconsistent. It is only sound for a database published as a static
    /// file set, which Orbit accepts when opening read-only storage.
    /// Observing current state again means opening a new connection.
    MainFileOnly,
}

/// Open an existing SQLite database for reads that must not write to it, and
/// must not create a WAL/SHM sidecar.
///
/// Two SQLite behaviors bound what is possible here. An ordinary read-only
/// connection stays current, but a WAL-mode database drives it through the
/// `-shm` wal-index, which SQLite creates — a write — when it is absent; on a
/// read-only mount that attempt fails outright. `immutable=1` needs no
/// sidecars, but it buys that by reading the main file alone and trusting it
/// never changes. Reporting either as current is how a read-only Orbit mount
/// turned an observation into `attempt to write a readonly database`, and how
/// a checkpointed database kept reporting its pre-open row count.
///
/// The database's own state decides which reads are possible. A published
/// main file without a wal-index is accepted as a degraded read; see
/// `observation_currency`.
pub fn open_observational(path: &Path) -> Result<OpenedConnection, OrbitError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| sqlite_path_error("inspect", path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(OrbitError::InvalidInput(format!(
            "SQLite database must be a regular file, not a symlink: {}",
            path.display()
        )));
    }
    // No SQL/pragma runs until the SQLite-owned header and sidecar set decide
    // whether a schema read could create a wal-index. Merely opening the normal
    // SQLite main-file handle does not request a read transaction or WAL access.
    let connection = open_read_only_connection(path, ObservationCurrency::Live)?;
    let currency = observation_currency(path, &connection)?;
    let connection = match currency {
        ObservationCurrency::Live => connection,
        ObservationCurrency::MainFileOnly => {
            // The bundled Unix VFS opens immutable MAIN_DB files through its
            // ordinary inode bookkeeping too; only the pager disables locking.
            // Both handles therefore retain SQLite's deferred-close discipline.
            drop(connection);
            open_read_only_connection(path, currency)?
        }
    };
    configure_observational_connection(&connection, path)?;

    // Opening is lazy, so an unreadable database or an unusable wal-index would
    // otherwise surface as an opaque failure inside whichever query happened to
    // run first.
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| {
            observational_unavailable(
                path,
                &format!("its first read failed: {}", sqlite_error_detail(&error)),
            )
        })?;

    if currency == ObservationCurrency::MainFileOnly {
        tracing::warn!(
            target: "orbit.common.sqlite",
            path = %path.display(),
            "no wal-index is published alongside the database; reading its main file alone, which \
             will not show commits made from writable storage after this open"
        );
    }

    Ok(OpenedConnection {
        connection,
        read_only: true,
        currency,
    })
}

/// Decide how `path` can be observed without writing to it, or explain why its
/// current state cannot be read at all.
///
/// The database file's own header decides first: a rollback-journal database
/// needs no shared memory, so an ordinary read-only connection reads it live
/// and creates nothing. A WAL-mode database needs the wal-index, and a
/// read-only connection may only use one that is already published, because
/// SQLite creates both the `-wal` and the `-shm` when either is missing.
///
/// That leaves a WAL-mode database whose sidecars are incomplete. A `-wal`
/// holding frames, or a `-shm` published without the `-wal` it indexes, may be
/// hiding committed pages the main file does not have; reporting the main file
/// alone would be a silent, stale success, so those always fail closed.
///
/// A WAL-mode database with no wal-index at all is accepted as the degraded
/// [`ObservationCurrency::MainFileOnly`] read. Nothing on disk proves it is
/// quiescent — sidecar absence only says no writer holds it *right now* — so
/// the caller receives the currency and a warning about the degraded read.
fn observation_currency(
    path: &Path,
    connection: &Connection,
) -> Result<ObservationCurrency, OrbitError> {
    let [wal_path, shm_path] = sqlite_sidecar_paths(path);
    let wal = sidecar_metadata(&wal_path)?;
    let shm = sidecar_metadata(&shm_path)?;

    if !database_uses_wal(connection, path)? {
        return Ok(ObservationCurrency::Live);
    }

    match (wal, shm) {
        (Some(_), Some(_)) => Ok(ObservationCurrency::Live),
        (None, Some(_)) => Err(observational_unavailable(
            path,
            "its '-shm' wal-index is published without the '-wal' sidecar it indexes",
        )),
        (Some(wal), None) if wal.len() > WAL_HEADER_BYTES => Err(observational_unavailable(
            path,
            "its '-wal' sidecar holds committed frames but its '-shm' wal-index is missing",
        )),
        (Some(_) | None, None) => Ok(ObservationCurrency::MainFileOnly),
    }
}

fn open_read_only_connection(
    path: &Path,
    currency: ObservationCurrency,
) -> Result<Connection, OrbitError> {
    let mut uri = url::Url::from_file_path(path).map_err(|()| {
        OrbitError::Store(format!(
            "cannot represent SQLite path '{}' as a file URI",
            path.display()
        ))
    })?;
    if currency == ObservationCurrency::MainFileOnly {
        uri.query_pairs_mut().append_pair("immutable", "1");
    }

    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| {
        sqlite_operation_error(
            Some(path),
            "cannot open SQLite database for observational reads",
            &error,
        )
    })
}

fn configure_observational_connection(conn: &Connection, path: &Path) -> Result<(), OrbitError> {
    pragma::set(conn, "query_only", &"ON")
        .map_err(|error| sqlite_operation_error(Some(path), "set query_only", &error))?;
    pragma::set(conn, "busy_timeout", &DEFAULT_BUSY_TIMEOUT_MS)
        .map_err(|error| sqlite_operation_error(Some(path), "set busy_timeout", &error))?;
    pragma::set(conn, "foreign_keys", &"ON")
        .map_err(|error| sqlite_operation_error(Some(path), "enable foreign keys", &error))?;
    Ok(())
}

/// The database's current state cannot be read from this process, and cannot
/// be repaired without writable storage. Name the writable step an operator
/// still owes rather than reporting the main file's older contents as current.
///
/// Deliberately not phrased as a read-only/permission failure: callers that
/// downgrade those to a warning and continue would turn this back into the
/// silent stale read it exists to prevent.
fn observational_unavailable(path: &Path, reason: &str) -> OrbitError {
    OrbitError::Store(format!(
        "cannot observe current SQLite state '{}': {reason}. Checkpoint the database from \
         writable storage (`PRAGMA wal_checkpoint(TRUNCATE)`) or publish its '-wal' and '-shm' \
         sidecars alongside it, then retry the observation",
        path.display()
    ))
}

/// Whether `path` resides on a filesystem mounted read-only.
///
/// SQLite can successfully open a database with read-write flags on such a
/// mount and only discover the restriction at its first real write. Detecting
/// the mount flag lets read paths select immutable mode before SQLite attempts
/// WAL/SHM sidecars or a pending schema migration.
#[cfg(unix)]
pub fn filesystem_is_read_only(path: &Path) -> Result<bool, OrbitError> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let path_bytes = path.as_os_str().as_bytes();
    let path = CString::new(path_bytes)
        .map_err(|_| OrbitError::Store("SQLite path contains an interior NUL byte".to_string()))?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a live NUL-terminated C string and `stats` points to
    // writable storage for one `statvfs` value. A zero return initializes it.
    let status = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
    if status != 0 {
        let error = std::io::Error::last_os_error();
        return Err(OrbitError::storage(
            StorageLayer::Store,
            crate::fs::io::is_readonly_or_access_error(&error),
            format!(
                "cannot inspect SQLite filesystem for '{}': {error}",
                path.to_string_lossy(),
            ),
        ));
    }
    // SAFETY: `statvfs` returned zero, so it initialized the output value.
    let stats = unsafe { stats.assume_init() };
    Ok(stats.f_flag & libc::ST_RDONLY != 0)
}

#[cfg(not(unix))]
pub fn filesystem_is_read_only(_path: &Path) -> Result<bool, OrbitError> {
    Ok(false)
}

/// Request WAL and report the journal mode SQLite settled on. Never fails:
/// WAL is a performance/concurrency upgrade, not a correctness requirement,
/// so refusals degrade to a warning plus the active mode.
fn request_wal_journal_mode(conn: &Connection, path: Option<&Path>) -> (String, bool) {
    match pragma::set_and_check::<String>(conn, "journal_mode", &"WAL") {
        Ok(mode) => {
            if !mode.eq_ignore_ascii_case("wal") && !mode.eq_ignore_ascii_case("memory") {
                tracing::warn!(
                    target: "orbit.common.sqlite",
                    journal_mode = mode.as_str(),
                    "requested WAL mode, but SQLite kept the active journal mode",
                );
            }
            (mode, false)
        }
        Err(error) => {
            let detail = sqlite_error_detail(&error);
            tracing::warn!(
                target: "orbit.common.sqlite",
                path = path.map(|path| path.display().to_string()),
                error = detail,
                "could not set WAL mode; continuing with the active journal mode",
            );
            let write_denied = is_readonly_or_access_sqlite_error(&error);
            (
                pragma::get::<String>(conn, "journal_mode")
                    .unwrap_or_else(|_| "unknown".to_string()),
                write_denied,
            )
        }
    }
}
