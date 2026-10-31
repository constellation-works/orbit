//! SQLite error translation into [`OrbitError`](crate::OrbitError).

use std::io;
use std::path::Path;

use crate::{OrbitError, SqliteContention, StorageLayer};

pub(super) fn sqlite_path_error(action: &str, path: &Path, error: io::Error) -> OrbitError {
    OrbitError::storage(
        StorageLayer::Store,
        crate::fs::io::is_readonly_or_access_error(&error),
        format!(
            "failed to {action} SQLite state '{}': {error}",
            path.display()
        ),
    )
}

/// Whether SQLite refused an operation because the database or its directory
/// is read-only or denies access: `SQLITE_READONLY` (and its extended codes),
/// `SQLITE_CANTOPEN` or `SQLITE_PERM`.
pub fn is_readonly_or_access_sqlite_error(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;

    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::ReadOnly | ErrorCode::CannotOpen | ErrorCode::PermissionDenied)
    )
}

/// Whether SQLite gave up on another connection's lock: `SQLITE_BUSY` or
/// `SQLITE_LOCKED` (and their extended codes).
pub(super) fn is_lock_busy_sqlite_error(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;

    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// A `layer` failure described by `message`, keeping whether SQLite refused
/// it as read-only or access-denied (see
/// [`is_readonly_or_access_sqlite_error`]) or lost it to a lock that outlasted
/// the busy timeout ([`OrbitError::StorageLockBusy`]). Use it wherever a
/// `rusqlite::Error` on a write or open path is wrapped into an
/// [`OrbitError`].
pub fn sqlite_error(
    layer: StorageLayer,
    error: &rusqlite::Error,
    message: impl Into<String>,
) -> OrbitError {
    if is_lock_busy_sqlite_error(error) {
        return OrbitError::StorageLockBusy {
            layer,
            message: message.into(),
        };
    }
    OrbitError::storage(layer, is_readonly_or_access_sqlite_error(error), message)
}

/// [`OrbitError::Store`] carrying `error`'s own text, classified as
/// [`sqlite_error`] does; the `map_err` form of `Store(e.to_string())`.
pub fn sqlite_store_error(error: rusqlite::Error) -> OrbitError {
    sqlite_error(StorageLayer::Store, &error, error.to_string())
}

pub(super) fn sqlite_operation_error(
    path: Option<&Path>,
    phase: &str,
    error: &rusqlite::Error,
) -> OrbitError {
    let detail = sqlite_error_detail(error);
    if is_lock_busy_sqlite_error(error)
        && let Some(path) = path
    {
        return OrbitError::SqliteContention(Box::new(SqliteContention {
            path: path.display().to_string(),
            phase: phase.to_string(),
            detail,
        }));
    }

    let operation = match phase {
        "set busy_timeout" => "failed to set busy_timeout",
        "enable foreign keys" => "failed to enable foreign keys",
        "set synchronous=NORMAL" => "failed to set synchronous=NORMAL",
        _ => phase,
    };
    let path = path.map_or_else(String::new, |path| format!(" for '{}'", path.display()));
    sqlite_error(
        StorageLayer::Store,
        error,
        format!("{operation}{path}: {detail}"),
    )
}

pub(super) fn sqlite_error_detail(error: &rusqlite::Error) -> String {
    match error.sqlite_error() {
        Some(sqlite) => format!(
            "{} [code={:?}, extended_code={}]",
            error, sqlite.code, sqlite.extended_code
        ),
        None => error.to_string(),
    }
}
