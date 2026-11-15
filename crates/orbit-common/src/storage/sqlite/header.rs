//! Bounded header reads through the connection's SQLite-owned file handle.

use std::path::Path;

use rusqlite::{Connection, ffi};

use crate::OrbitError;

use super::errors::sqlite_operation_error;

const DATABASE_MAGIC: &[u8] = b"SQLite format 3\0";
const DATABASE_HEADER_BYTES: usize = 20;
const WAL_FILE_FORMAT: u8 = 2;

/// Inspect a freshly opened, exclusively owned connection before any SQL runs.
///
/// Do not open a second ordinary file descriptor here: closing it could cancel
/// another connection's POSIX locks. SQLite retains ownership of this handle,
/// including its VFS's close coordination. No pager/schema read is requested.
pub(super) fn database_uses_wal(connection: &Connection, path: &Path) -> Result<bool, OrbitError> {
    let mut file = std::ptr::null_mut::<ffi::sqlite3_file>();
    // SAFETY: the fresh connection remains alive and is not shared or running
    // statements/callbacks. FILE_POINTER writes one borrowed sqlite3_file pointer
    // into `file`; we neither detach nor close that SQLite-owned handle.
    let status = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            std::ptr::from_mut(&mut file).cast(),
        )
    };
    if status != ffi::SQLITE_OK {
        return Err(header_error(path, status));
    }
    // SAFETY: FILE_POINTER succeeded; both pointers are borrowed only while
    // their owning connection is alive. A VFS without a readable main file is
    // refused rather than falling back to a pathname open.
    let read = unsafe { file.as_ref().and_then(|file| file.pMethods.as_ref()) }
        .and_then(|methods| methods.xRead)
        .ok_or_else(|| {
            OrbitError::Store(format!(
                "cannot inspect SQLite database '{}': its VFS has no readable main-file handle",
                path.display()
            ))
        })?;
    let mut header = [0_u8; DATABASE_HEADER_BYTES];
    // SAFETY: xRead belongs to this live file; the buffer has exactly the
    // requested 20 writable bytes, and the nonnegative offset is zero.
    let status = unsafe { read(file, header.as_mut_ptr().cast(), 20, 0) };
    match status {
        // Preserve the previous short/truncated/non-SQLite header policy:
        // SQLite's first actual query supplies the database-format diagnosis.
        ffi::SQLITE_IOERR_SHORT_READ => Ok(false),
        ffi::SQLITE_OK => Ok(header.starts_with(DATABASE_MAGIC)
            && (header[18] == WAL_FILE_FORMAT || header[19] == WAL_FILE_FORMAT)),
        status => Err(header_error(path, status)),
    }
}

fn header_error(path: &Path, status: i32) -> OrbitError {
    sqlite_operation_error(
        Some(path),
        "read SQLite-owned database header",
        &rusqlite::Error::SqliteFailure(ffi::Error::new(status), None),
    )
}
