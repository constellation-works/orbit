use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

#[cfg(unix)]
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Creates a directory tree for secret-bearing Orbit state.
///
/// On Unix, every directory this call creates is immediately restricted to the
/// current user (`0o700`) instead of relying on the process umask. Existing
/// directories are left unchanged so callers do not unexpectedly chmod a
/// workspace root or home directory.
#[allow(
    clippy::disallowed_methods,
    reason = "non-Unix fallback for the private-directory helper; Unix uses an explicit owner-only mode"
)]
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        create_private_dir_all_unix(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

/// Creates one secret-bearing Orbit state directory without relying on umask.
///
/// Unlike [`create_private_dir_all`], this preserves [`fs::create_dir`]'s
/// exclusive-create behavior and returns `AlreadyExists` for an existing path.
#[allow(
    clippy::disallowed_methods,
    reason = "non-Unix fallback for the private-directory helper; Unix uses an explicit owner-only mode"
)]
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIR_MODE);
        builder.create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

/// Create a new secret-bearing file for writing.
///
/// On Unix, the file is opened with and then set to `0o600` so group/other bits
/// cannot leak in through the process umask.
pub(crate) fn create_new_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).truncate(true).write(true);
    open_private_file(path, &mut options)
}

/// Open a secret-bearing append-only file, creating it if needed.
///
/// On Unix, newly created and pre-existing files are set to `0o600`.
pub(crate) fn append_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    open_private_file(path, &mut options)
}

/// Creates a directory symlink `dst` → `src`. Platform-abstracted over
/// Unix (`symlink`) and Windows (`symlink_dir`).
#[cfg(unix)]
pub fn create_dir_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
pub fn create_dir_symlink(src: &Path, dst: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}
pub(crate) fn open_private_file(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    let path = validated_private_file_path(path)?;
    apply_private_file_mode(options);
    apply_no_follow_final_component(options);
    let file = options.open(&path)?;
    super::set_private_file_permissions_for_open_file(&file)?;
    Ok(file)
}

/// Open `path` for a read-only inspection without following its final component.
///
/// On Unix the open is nonblocking as well as no-follow, so a FIFO swapped in
/// after a caller's pathname check cannot indefinitely block the reader. The
/// caller remains responsible for checking the opened descriptor's type and
/// mapping errors into its domain-specific behavior.
pub fn open_read_only_no_follow(path: &Path) -> io::Result<File> {
    let path = validated_private_file_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    apply_read_only_no_follow(&mut options);
    options.open(&path)
}

/// Resolve a private file's parent before opening it and reject a final
/// component that would redirect the operation through a symlink.
///
/// Callers intentionally use symlinked parent directories for checkout
/// projections, so the parent is canonicalized rather than rejected. The
/// final component is checked without following it, and the open below also
/// uses `O_NOFOLLOW` on Unix so the check and open cannot be raced into a
/// different file.
fn validated_private_file_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no file name", path.display()),
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no parent directory", path.display()),
        )
    })?;
    let canonical_parent = fs::canonicalize(parent)?;
    let canonical_path = canonical_parent.join(file_name);

    match fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private file path must not be a symlink: {}",
                    path.display()
                ),
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private file path must be a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(canonical_path)
}

#[cfg(unix)]
fn apply_no_follow_final_component(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(unix)]
fn apply_read_only_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
}

#[cfg(windows)]
fn apply_read_only_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn apply_read_only_no_follow(_options: &mut OpenOptions) {}

#[cfg(not(unix))]
fn apply_no_follow_final_component(_options: &mut OpenOptions) {}

/// Restrict an already-open file to owner-only access.
///
/// A descriptor whose file is already at the private mode is left alone: the
/// `fchmod` would be a no-op for permissions, but it still counts as a metadata
/// change, and on macOS the next `close` of a descriptor that issued one costs
/// milliseconds. Lock files are opened once per task bundle on every listing
/// and search, so re-asserting the mode on each open dominated those commands.
pub(crate) fn set_private_file_permissions_for_open_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if has_private_file_mode(&file.metadata()?) {
            return Ok(());
        }
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(())
    }
}

#[cfg(unix)]
fn create_private_dir_all_unix(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }

        match fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => continue,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a directory", current.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match create_private_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        if !current.is_dir() {
                            return Err(io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                format!("{} exists and is not a directory", current.display()),
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

#[cfg(unix)]
fn apply_private_file_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(PRIVATE_FILE_MODE);
}

#[cfg(not(unix))]
fn apply_private_file_mode(_options: &mut OpenOptions) {}

/// Restrict an existing regular file to owner-only access without following
/// a symlink at its final component.
///
/// A symlink or other irregular file is refused and its target left untouched.
/// Existing SQLite files require path-only handles, not ordinary data
/// descriptors: closing the latter could cancel another SQLite connection's
/// POSIX locks. Linux x86-64 uses O_PATH and fchmodat2(AT_EMPTY_PATH); other Unix
/// systems refuse repairs before opening a data descriptor. Already-private
/// files never need a handle. Generic lock/secret-file hardening stays separate.
#[cfg(feature = "sqlite")]
pub(crate) fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    set_sqlite_file_permissions(path, false)
}

/// Remove group/other access without adding owner write permission to a
/// read-only SQLite database or sidecar.
#[cfg(all(unix, feature = "sqlite"))]
pub(crate) fn set_read_only_private_file_permissions(path: &Path) -> io::Result<()> {
    set_sqlite_file_permissions(path, true)
}

#[cfg(feature = "sqlite")]
fn set_sqlite_file_permissions(path: &Path, preserve_owner_mode: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure_regular_private_file(path, &metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = sqlite_private_mode(&metadata, preserve_owner_mode);
        if metadata.permissions().mode() & 0o7777 == mode {
            return Ok(());
        }
        let file = super::open_sqlite_permission_handle_no_follow(path)?;
        super::set_private_permissions_for_sqlite_handle(&file, preserve_owner_mode)
    }
    #[cfg(not(unix))]
    {
        let _ = preserve_owner_mode;
        Ok(())
    }
}

#[cfg(all(unix, feature = "sqlite"))]
fn sqlite_private_mode(metadata: &fs::Metadata, preserve_owner_mode: bool) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    if preserve_owner_mode {
        metadata.permissions().mode() & 0o700
    } else {
        PRIVATE_FILE_MODE
    }
}

/// Pin an existing SQLite inode without acquiring a data descriptor.
///
/// The fstat/type check rejects an O_PATH handle to a symlink itself; it never
/// follows it. Closing an O_PATH handle does not invoke Linux's POSIX-lock
/// removal for an ordinary data-file close.
#[cfg(all(unix, feature = "sqlite"))]
pub(crate) fn open_sqlite_permission_handle_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        ensure_regular_private_file(path, &metadata)?;
        Ok(file)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = path;
        Err(unsupported_sqlite_permission_repair())
    }
}

/// Apply the mode to the verified pinned inode, never a newly resolved path.
#[cfg(all(unix, feature = "sqlite"))]
pub(crate) fn set_private_permissions_for_sqlite_handle(
    file: &File,
    preserve_owner_mode: bool,
) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;

        let metadata = file.metadata()?;
        let mode = sqlite_private_mode(&metadata, preserve_owner_mode);
        if metadata.permissions().mode() & 0o7777 == mode {
            return Ok(());
        }
        // SAFETY: this O_PATH descriptor pins a type-checked regular inode.
        // AT_EMPTY_PATH applies only to it; the empty string is NUL-terminated.
        // Use the syscall directly, never a libc /proc fallback that might
        // open/close an ordinary descriptor to this database inode.
        let status = unsafe {
            libc::syscall(
                libc::SYS_fchmodat2,
                file.as_raw_fd(),
                c"".as_ptr(),
                mode as libc::mode_t,
                libc::AT_EMPTY_PATH,
            )
        };
        if status == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
        ) {
            return Err(unsupported_sqlite_permission_repair());
        }
        Err(error)
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = (file, preserve_owner_mode);
        Err(unsupported_sqlite_permission_repair())
    }
}

#[cfg(all(unix, feature = "sqlite"))]
fn unsupported_sqlite_permission_repair() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "safe SQLite permission repair is unavailable on this platform or kernel; stop every \
         process using the database, restrict the database and its existing sidecars to \
         owner-only permissions offline, then retry (retain owner read-only mode when applicable)",
    )
}

#[cfg(feature = "sqlite")]
fn ensure_regular_private_file(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "private file path must not be a symlink: {}",
                path.display()
            ),
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "private file path must be a regular file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn has_private_file_mode(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o7777 == PRIVATE_FILE_MODE
}

/// Attributable write-access failure, or `None` when `err` is some other I/O.
///
/// Shared by [`crate::OrbitError::from_write_io`] and lock acquisition so
/// EROFS/EACCES always names `path` and hints at a sandbox/environment
/// condition instead of a store defect.
pub(crate) fn write_access_error_message(path: &Path, err: &io::Error) -> Option<String> {
    is_readonly_or_access_error(err).then(|| {
        format!(
            "`{}` is not writable: {err}; this is likely a sandbox or environment condition, not an Orbit store defect",
            path.display()
        )
    })
}
/// True when `error` is a read-only filesystem or access denial.
///
/// Matches both [`io::ErrorKind`] and the raw Unix errno so callers do not
/// have to re-derive EROFS/EACCES classification. Used to distinguish a
/// sandbox or environment mount from a store defect.
pub fn is_readonly_or_access_error(error: &io::Error) -> bool {
    match error.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => true,
        _ => error
            .raw_os_error()
            .is_some_and(is_readonly_or_access_errno),
    }
}

#[cfg(unix)]
fn is_readonly_or_access_errno(code: i32) -> bool {
    code == libc::EROFS || code == libc::EACCES
}

#[cfg(not(unix))]
fn is_readonly_or_access_errno(_code: i32) -> bool {
    false
}
