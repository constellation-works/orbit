//! Shared SQLite connection defaults for every Orbit SQLite store.
//!
//! Historically each store (orbit-store `Store`, its ID allocator and task
//! registry, and orbit-search's `VectorStore`) hand-rolled its own pragma
//! setup, and the copies drifted (missing `foreign_keys` here, missing
//! `busy_timeout` there). [`apply_default_pragmas`] is the single source of
//! truth: call it on every freshly opened connection, then layer any
//! store-specific overrides (e.g. the task registry's `synchronous=FULL`)
//! on top.

use std::fs;
use std::io;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::OrbitError;

/// Default `busy_timeout` applied to every Orbit SQLite connection, in
/// milliseconds. Writers under WAL still serialize; this bounds how long a
/// contending connection spins before surfacing `database is locked`.
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 5_000;

/// Bytes in a WAL file header. A `-wal` of at most this size carries no
/// frames, so the main database file already holds every committed page.
const WAL_HEADER_BYTES: u64 = 32;

/// Leading bytes of every SQLite database file.
const DATABASE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Bytes of the database header this module reads: through offsets 18 and 19,
/// the write and read file-format versions.
const DATABASE_HEADER_BYTES: u64 = 20;

/// File-format version recorded at offsets 18 and 19 while a database is in
/// WAL mode. Rollback-journal databases record `1`.
const WAL_FILE_FORMAT: u8 = 2;

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
/// pre-existing sidecars are repaired as part of the same operation. Newly
/// created parent directories are owner-only as well. An existing read-only
/// database on writable storage has group/other permissions removed before it
/// is opened for observation. A database on a read-only filesystem is opened
/// observationally before any directory creation or permission change is
/// attempted; see [`open_observational`] for how such a database is read.
pub fn open_private(path: &Path) -> Result<OpenedConnection, OrbitError> {
    let path = validated_sqlite_path(path)?;

    match fs::metadata(&path) {
        Ok(metadata) => {
            let filesystem_read_only = filesystem_is_read_only(&path)?;
            if filesystem_read_only || metadata.permissions().readonly() {
                return open_private_read_only(&path, filesystem_read_only);
            }
            harden_sqlite_files(&path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(sqlite_path_error("inspect", &path, error)),
    }

    prepare_private_database_file(&path)?;

    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| {
        OrbitError::Store(format!(
            "cannot open SQLite database '{}': {error}",
            path.display()
        ))
    })?;
    let pragmas = apply_default_pragmas(&connection)?;
    if pragmas.write_denied || filesystem_is_read_only(&path)? {
        drop(connection);
        return open_observational(&path, ObservationRequirement::PublishedMainFile);
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
pub(super) fn open_private_read_only(
    path: &Path,
    filesystem_read_only: bool,
) -> Result<OpenedConnection, OrbitError> {
    let path = validated_sqlite_path(path)?;

    if !filesystem_read_only {
        harden_read_only_sqlite_files(&path)?;
    }
    open_observational(&path, ObservationRequirement::PublishedMainFile)
}

/// Resolve a SQLite file path through its existing parent and reject traversal
/// and final-component symlinks before any SQLite or database-file permission
/// operation.
///
/// Orbit callers provide complete paths because the database may live in a
/// caller-selected state root. The root remains caller-owned, but path
/// traversal and symlink redirection are not part of that contract. Creating
/// missing parents preserves the existing first-open behavior; once they exist,
/// all subsequent operations use the canonical parent path.
fn validated_sqlite_path(path: &Path) -> Result<PathBuf, OrbitError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(OrbitError::InvalidInput(format!(
            "SQLite path '{}' must not contain parent-directory traversal",
            path.display()
        )));
    }

    let file_name = path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "SQLite path '{}' must name a database file",
            path.display()
        ))
    })?;
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "SQLite path '{}' must have a parent directory",
            path.display()
        ))
    })?;

    create_private_dir_all(parent)?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| sqlite_path_error("resolve parent for", parent, error))?;
    let canonical_path = canonical_parent.join(file_name);

    if !canonical_path.starts_with(&canonical_parent) {
        return Err(OrbitError::InvalidInput(format!(
            "SQLite path '{}' escapes its parent directory",
            path.display()
        )));
    }

    match fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OrbitError::InvalidInput(format!(
                "SQLite path must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(OrbitError::InvalidInput(format!(
                "SQLite path must be a regular file: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(sqlite_path_error("inspect", &canonical_path, error)),
    }

    Ok(canonical_path)
}

/// Create a sensitive SQLite-adjacent state directory.
///
/// On Unix, directories created by this call are `0o700`; existing ancestors
/// are deliberately left unchanged.
pub fn create_private_dir_all(path: &Path) -> Result<(), OrbitError> {
    crate::fs::io::create_private_dir_all(path)
        .map_err(|error| sqlite_path_error("create private directory", path, error))
}

fn prepare_private_database_file(path: &Path) -> Result<(), OrbitError> {
    match crate::fs::io::create_new_private_file(path) {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            crate::fs::io::set_private_file_permissions(path)
                .map_err(|error| sqlite_path_error("harden", path, error))
        }
        Err(error) => Err(sqlite_path_error("create", path, error)),
    }
}

fn harden_sqlite_files(path: &Path) -> Result<(), OrbitError> {
    harden_existing_file(path)?;
    for sidecar in sqlite_sidecar_paths(path) {
        harden_existing_file(&sidecar)?;
    }
    Ok(())
}

fn harden_existing_file(path: &Path) -> Result<(), OrbitError> {
    match crate::fs::io::set_private_file_permissions(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(sqlite_path_error("harden", path, error)),
    }
}

fn harden_read_only_sqlite_files(path: &Path) -> Result<(), OrbitError> {
    harden_existing_read_only_file(path)?;
    for sidecar in sqlite_sidecar_paths(path) {
        harden_existing_read_only_file(&sidecar)?;
    }
    Ok(())
}

#[cfg(unix)]
fn harden_existing_read_only_file(path: &Path) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    let file = match open_read_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(sqlite_path_error("inspect", path, error)),
    };

    let owner_only_mode = file
        .metadata()
        .map_err(|error| sqlite_path_error("inspect", path, error))?
        .permissions()
        .mode()
        & 0o700;
    file.set_permissions(fs::Permissions::from_mode(owner_only_mode))
        .map_err(|error| sqlite_path_error("harden", path, error))
}

#[cfg(not(unix))]
fn harden_existing_read_only_file(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// Open a database or sidecar file for reading without traversing a final
/// symlink, so inspection stays inside the directory
/// [`validated_sqlite_path`] resolved.
#[cfg(unix)]
fn open_read_no_follow(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_no_follow(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

fn sqlite_sidecar_paths(path: &Path) -> [PathBuf; 2] {
    [
        path_with_suffix(path, "-wal"),
        path_with_suffix(path, "-shm"),
    ]
}

/// Metadata for an existing sidecar, or `None` when it is absent.
///
/// A symlinked or otherwise irregular sidecar is rejected: read-only opens
/// decide what to trust from these files, and SQLite would follow a link out
/// of the state directory [`validated_sqlite_path`] just checked.
fn sidecar_metadata(path: &Path) -> Result<Option<fs::Metadata>, OrbitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(OrbitError::InvalidInput(format!(
            "SQLite sidecar must be a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(sqlite_path_error("inspect", path, error)),
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn sqlite_path_error(action: &str, path: &Path, error: io::Error) -> OrbitError {
    OrbitError::Store(format!(
        "failed to {action} SQLite state '{}': {error}",
        path.display()
    ))
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

impl PragmaOutcome {
    /// True when the connection ended up in WAL mode. In-memory databases
    /// report `memory` and return false; callers that require WAL can turn
    /// that into a hard error.
    pub fn wal_active(&self) -> bool {
        self.journal_mode.eq_ignore_ascii_case("wal")
    }
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
///   `FULL` after calling this.
pub fn apply_default_pragmas(conn: &Connection) -> Result<PragmaOutcome, OrbitError> {
    let (journal_mode, mut write_denied) = request_wal_journal_mode(conn);
    conn.pragma_update(None, "busy_timeout", DEFAULT_BUSY_TIMEOUT_MS)
        .map_err(|e| OrbitError::Store(format!("failed to set busy_timeout: {e}")))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| OrbitError::Store(format!("failed to enable foreign keys: {e}")))?;
    if let Err(error) = conn.pragma_update(None, "synchronous", "NORMAL") {
        let mapped = OrbitError::Store(format!("failed to set synchronous=NORMAL: {error}"));
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

/// What a caller needs from an observational open.
///
/// A database whose current state cannot be read without writing leaves only a
/// degraded read, and which of the two answers is right — refuse, or read the
/// main file alone — belongs to the caller, not to whichever sidecars happen
/// to be on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationRequirement {
    /// The connection must observe commits made after it is opened, or the
    /// open must fail explaining what writable storage still owes it.
    CurrentState,
    /// The caller also accepts [`ObservationCurrency::MainFileOnly`], having
    /// weighed that degraded read against not reading the database at all.
    PublishedMainFile,
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
    /// file set, and only a caller that passes
    /// [`ObservationRequirement::PublishedMainFile`] ever receives it.
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
/// The database's own state decides which reads are possible, and
/// `requirement` decides whether the degraded one is acceptable; see
/// [`observation_currency`].
pub fn open_observational(
    path: &Path,
    requirement: ObservationRequirement,
) -> Result<OpenedConnection, OrbitError> {
    let currency = observation_currency(path, requirement)?;
    let connection = open_read_only_connection(path, currency)?;

    // Opening is lazy, so an unreadable database or an unusable wal-index would
    // otherwise surface as an opaque failure inside whichever query happened to
    // run first.
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| {
            observational_unavailable(path, &format!("its first read failed: {error}"))
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
/// A WAL-mode database with no wal-index at all is the one case the caller
/// decides. Nothing on disk proves it is quiescent — sidecar absence only says
/// no writer holds it *right now* — so the degraded
/// [`ObservationCurrency::MainFileOnly`] read is offered to
/// [`ObservationRequirement::PublishedMainFile`] and refused to
/// [`ObservationRequirement::CurrentState`].
pub(super) fn observation_currency(
    path: &Path,
    requirement: ObservationRequirement,
) -> Result<ObservationCurrency, OrbitError> {
    let [wal_path, shm_path] = sqlite_sidecar_paths(path);
    let wal = sidecar_metadata(&wal_path)?;
    let shm = sidecar_metadata(&shm_path)?;

    if !database_uses_wal(path)? {
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
        (Some(_) | None, None) => match requirement {
            ObservationRequirement::PublishedMainFile => Ok(ObservationCurrency::MainFileOnly),
            ObservationRequirement::CurrentState => Err(observational_unavailable(
                path,
                "it is in WAL mode with no published '-shm' wal-index, so a reader that may not \
                 create one can only read the main file alone",
            )),
        },
    }
}

/// Whether the database file records WAL mode in its header.
///
/// A missing or truncated file, and any file that does not carry the SQLite
/// magic, is reported as not WAL: there are no committed WAL frames to miss,
/// and SQLite gives a better diagnosis of the file itself than this check
/// could.
fn database_uses_wal(path: &Path) -> Result<bool, OrbitError> {
    let file = match open_read_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(sqlite_path_error("inspect", path, error)),
    };

    let mut header = Vec::with_capacity(DATABASE_HEADER_BYTES as usize);
    file.take(DATABASE_HEADER_BYTES)
        .read_to_end(&mut header)
        .map_err(|error| sqlite_path_error("read the header of", path, error))?;
    if header.len() < DATABASE_HEADER_BYTES as usize || !header.starts_with(DATABASE_MAGIC) {
        return Ok(false);
    }

    Ok(header[18] == WAL_FILE_FORMAT || header[19] == WAL_FILE_FORMAT)
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

    let conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| {
        OrbitError::Store(format!(
            "cannot open SQLite database '{}' for observational reads: {error}",
            path.display()
        ))
    })?;

    conn.pragma_update(None, "query_only", "ON")
        .map_err(|error| OrbitError::Store(format!("failed to set query_only: {error}")))?;
    conn.pragma_update(None, "busy_timeout", DEFAULT_BUSY_TIMEOUT_MS)
        .map_err(|error| OrbitError::Store(format!("failed to set busy_timeout: {error}")))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| OrbitError::Store(format!("failed to enable foreign keys: {error}")))?;
    Ok(conn)
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
        return Err(OrbitError::Store(format!(
            "cannot inspect SQLite filesystem for '{}': {}",
            path.to_string_lossy(),
            std::io::Error::last_os_error()
        )));
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
fn request_wal_journal_mode(conn: &Connection) -> (String, bool) {
    match conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0)) {
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
            tracing::warn!(
                target: "orbit.common.sqlite",
                error = %error,
                "could not set WAL mode; continuing with the active journal mode",
            );
            let write_denied = OrbitError::Store(error.to_string()).is_readonly_or_access_failure();
            (
                conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                    .unwrap_or_else(|_| "unknown".to_string()),
                write_denied,
            )
        }
    }
}
