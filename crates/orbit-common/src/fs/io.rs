//! Atomic filesystem primitives.
//!
//! Consolidates the variants that historically existed across the workspace:
//! - `orbit-core::fs_utils::atomic_write_text` (volatile)
//! - `orbit-store::file::fs_utils::write_atomic` (volatile, with separate flock helper)
//! - the former `orbit-knowledge` durable write (parent-dir fsync), since removed
//!
//! The durable variant is the canonical one: rename-into-place plus
//! parent-directory fsync so the rename itself is flushed. Volatile is
//! offered for hot paths where the caller accepts post-crash inconsistency.
//!
//! All functions return `io::Result`; callers map to their domain error type
//! (`OrbitError`, `KnowledgeError`, etc.) at the boundary. Keeping this
//! module domain-free preserves the `types::` / `utility::` split inside
//! `orbit-common`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::file_lock::acquire_shared_file_lock;
pub use super::file_lock::{
    DEFAULT_FILE_LOCK_TIMEOUT, FileLockGuard, FileLockHolderInfo, FileLockOptions, FileLockTimeout,
    acquire_exclusive_file_lock, read_file_lock_holder, try_acquire_exclusive_file_lock,
};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        create_private_dir_all_unix(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
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

/// Atomically write `content` to `path`, then fsync the parent directory so
/// the rename survives a crash. Creates parent directories as needed.
pub fn atomic_write_text(path: &Path, content: &str) -> io::Result<()> {
    atomic_write_bytes(path, content.as_bytes())
}

/// Atomically write `content` bytes to `path`, then fsync the parent directory
/// so the rename survives a crash. Creates parent directories as needed.
pub fn atomic_write_bytes(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut staged = StagedTextFile::new_internal(path, content, true)?;
    staged.commit()
}

/// Atomically write secret-bearing bytes to `path` with private file
/// permissions, even when replacing an existing file with broader permissions.
pub(crate) fn atomic_write_private_bytes(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut staged = StagedTextFile::new_internal_with_permissions(path, content, true, false)?;
    staged.commit()
}

/// Atomically write `content` to `path` without fsyncing the parent.
/// Cheaper than [`atomic_write_text`] but post-crash the rename may be lost.
pub fn atomic_write_text_volatile(path: &Path, content: &str) -> io::Result<()> {
    let mut staged = StagedTextFile::new_internal(path, content.as_bytes(), false)?;
    staged.commit()
}

/// A staged write that can be committed or dropped. Useful when a caller
/// needs to perform additional validation between staging and commit.
///
/// Drop before `commit()` removes the temp file.
pub struct StagedTextFile {
    target_path: PathBuf,
    temp_path: PathBuf,
    parent_dir: Option<File>,
    sync_parent: bool,
    committed: bool,
}

impl StagedTextFile {
    /// Stage a durable write. `commit()` renames and fsyncs the parent dir.
    pub fn new(target_path: &Path, content: &str) -> io::Result<Self> {
        Self::new_internal(target_path, content.as_bytes(), true)
    }

    /// Stage a volatile write. `commit()` renames without fsyncing.
    pub fn new_volatile(target_path: &Path, content: &str) -> io::Result<Self> {
        Self::new_internal(target_path, content.as_bytes(), false)
    }

    fn new_internal(target_path: &Path, content: &[u8], durable: bool) -> io::Result<Self> {
        Self::new_internal_with_permissions(target_path, content, durable, true)
    }

    fn new_internal_with_permissions(
        target_path: &Path,
        content: &[u8],
        durable: bool,
        preserve_existing_permissions: bool,
    ) -> io::Result<Self> {
        Self::stage_with(
            target_path,
            durable,
            preserve_existing_permissions,
            |file| file.write_all(content),
        )
    }

    fn stage_with<F>(
        target_path: &Path,
        durable: bool,
        preserve_existing_permissions: bool,
        write: F,
    ) -> io::Result<Self>
    where
        F: FnOnce(&mut File) -> io::Result<()>,
    {
        let parent = target_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no parent dir for {}", target_path.display()),
            )
        })?;
        create_private_dir_all(parent)?;
        let canonical_target = validated_atomic_target(target_path)?;
        let canonical_parent = canonical_target.parent().ok_or_else(|| {
            io::Error::other("validated atomic target is missing its parent directory")
        })?;

        let temp_path = temp_path_for(&canonical_target);
        let mut file = create_new_private_file(&temp_path)?;
        let mut cleanup = TempFileCleanup::new(temp_path.clone());

        if preserve_existing_permissions && let Ok(metadata) = fs::metadata(&canonical_target) {
            fs::set_permissions(&temp_path, metadata.permissions())?;
        }

        write(&mut file)?;
        if durable {
            file.sync_all()?;
        }
        drop(file);

        let parent_dir = durable.then(|| File::open(canonical_parent)).transpose()?;

        cleanup.disarm();

        Ok(Self {
            target_path: canonical_target,
            temp_path,
            parent_dir,
            sync_parent: durable,
            committed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn stage_with_for_test<F>(target_path: &Path, write: F) -> io::Result<Self>
    where
        F: FnOnce(&mut File) -> io::Result<()>,
    {
        Self::stage_with(target_path, true, true, write)
    }

    pub fn commit(&mut self) -> io::Result<()> {
        fs::rename(&self.temp_path, &self.target_path)?;
        self.committed = true;
        if self.sync_parent {
            let Some(parent_dir) = self.parent_dir.as_ref() else {
                return Err(io::Error::other(
                    "durable staged file is missing its parent directory handle",
                ));
            };
            sync_parent_dir(parent_dir)?;
        }
        Ok(())
    }
}

/// Resolve an atomic write target to a canonical parent and a single file
/// component.
///
/// The resolved parent becomes the containment root for the final rename.
/// Rejecting the dot components before creating a staging file also prevents a
/// path such as `parent/..` from being interpreted as the parent directory
/// itself.
fn validated_atomic_target(path: &Path) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("atomic write path has no file name: {}", path.display()),
        ));
    };
    if file_name == "." || file_name == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("atomic write path must name a file: {}", path.display()),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no parent dir for {}", path.display()),
        )
    })?;
    let parent_for_resolution = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = fs::canonicalize(parent_for_resolution)?;
    let canonical_target = canonical_parent.join(file_name);
    if !canonical_target.starts_with(&canonical_parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("atomic write target escapes its parent: {}", path.display()),
        ));
    }

    Ok(canonical_target)
}

/// Removes a newly-created staging file if setup or writing fails before the
/// staged file can take ownership of cleanup.
struct TempFileCleanup {
    path: PathBuf,
    armed: bool,
}

impl TempFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for StagedTextFile {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(&self.temp_path);
    }
}

fn temp_path_for(target_path: &Path) -> PathBuf {
    let file_name = target_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("orbit");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".{file_name}.{nanos}.{counter}.tmp");
    target_path.with_file_name(temp_name)
}

/// Fsync an already-open parent directory so its directory entries are durable.
///
/// fsync on a file or directory persists that object's own data and inode, but
/// not the entry in its *parent* that makes it reachable by path. After freshly
/// creating a file, directory, or rename target, a crash can otherwise leave a
/// fully-fsynced but unreferenced inode that recovery reclaims as an orphan.
/// Call this with the directory handle for the parent of a newly created path
/// to close that window. Taking a handle keeps path resolution at the caller's
/// trusted filesystem boundary instead of resolving a caller-provided path in
/// this synchronization primitive.
pub fn sync_parent_dir(parent_dir: &File) -> io::Result<()> {
    parent_dir.sync_all()
}

// ---------------------------------------------------------------------------
// Filesystem helpers beyond atomic write
// ---------------------------------------------------------------------------

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

/// Removes `path` if it exists, tolerating missing paths. Symlinks are
/// unlinked without following; directories are removed recursively.
pub fn remove_path_if_exists(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if metadata.file_type().is_symlink() {
        fs::remove_file(path)
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Writes `content` to `path`, creating parent directories as needed. Not
/// atomic — for crash-safe writes use [`atomic_write_text`].
pub fn write_text_with_parent(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

thread_local! {
    /// Lock files this thread currently holds, by lock-file path.
    ///
    /// ORB-10988: `flock(2)` is owned by the open file description, not by the
    /// process or thread, so a nested `with_exclusive_file_lock` on the same
    /// path opens a *second* descriptor and blocks against the outer one —
    /// a self-deadlock, not a re-entry. Tracking held paths per thread makes
    /// the helper re-entrant, which is what lets a caller hold a task lock
    /// across a read-modify-write whose inner writes lock the same file.
    static HELD_LOCK_PATHS: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// Removes `path` from this thread's held set on drop, including on unwind.
struct HeldLockPath(PathBuf);

impl Drop for HeldLockPath {
    fn drop(&mut self) {
        HELD_LOCK_PATHS.with(|held| {
            held.borrow_mut().remove(&self.0);
        });
    }
}

/// Registers `path` as held by this thread, or returns `None` when this thread
/// already holds it (the caller then runs `op` under the outer lock).
fn claim_lock_path(path: &Path) -> Option<HeldLockPath> {
    HELD_LOCK_PATHS.with(|held| {
        held.borrow_mut()
            .insert(path.to_path_buf())
            .then(|| HeldLockPath(path.to_path_buf()))
    })
}

/// Run `op` while holding an exclusive advisory flock on a sibling lock
/// file of `target_path` (`.<filename>.lock`). Creates the parent directory
/// if missing. The lock is released when this function returns.
///
/// The lock is re-entrant per thread: a nested call for the same lock path
/// runs `op` directly under the outermost acquisition instead of deadlocking
/// on a second descriptor. Cross-thread and cross-process callers wait up to
/// [`DEFAULT_FILE_LOCK_TIMEOUT`] on the flock, including readers holding
/// [`with_shared_file_lock`] on the same target.
///
/// The closure returns `Result<T, E>` where any filesystem error hit while
/// acquiring the lock is folded into `E` via `From<std::io::Error>` —
/// callers returning `OrbitError`, `io::Error`, or any error type that
/// implements `From<io::Error>` compose directly.
///
/// `label` prefixes error messages for diagnosability when the lock path
/// alone isn't enough context.
pub fn with_exclusive_file_lock<T, E, F>(target_path: &Path, label: &str, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_exclusive_file_lock_options(target_path, label, FileLockOptions::default(), op)
}

/// [`with_exclusive_file_lock`] with an explicit, testable acquisition policy.
pub fn with_exclusive_file_lock_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let parent = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine parent for '{}'", target_path.display()),
        )
    })?;

    // Create the parent before resolving, not after. `resolved_lock_path`
    // canonicalizes through the parent and falls back to the literal path when
    // the parent is missing, so resolving first made the key depend on whether
    // this call happened to be the one that created the directory: an outer
    // call keyed the literal path, created the parent, and the nested call then
    // canonicalized to a different key, missed the lock it already held, and
    // blocked on a second descriptor to the same file. Creating the parent
    // first makes the parent always resolvable, so every call in a nest agrees
    // on the key. Under a path that canonicalizes to itself the two orders are
    // indistinguishable, which is why this only ever deadlocked where a symlink
    // sat above the target.
    create_private_dir_all(parent).map_err(|e| classify_lock_io(parent, e))?;
    let lock_path = resolved_lock_path(target_path)?;
    let Some(_held) = claim_lock_path(&lock_path) else {
        return op();
    };
    let _lock = acquire_exclusive_file_lock(&lock_path, label, options)?;

    op()
}

/// Run `op` while holding a *shared* advisory flock on the same sibling lock
/// file [`with_exclusive_file_lock`] uses, so readers exclude writers of that
/// target while staying concurrent with each other.
///
/// This is the read half of a multi-file critical section (ORB-11349): a task
/// bundle's transition appends to one file and republishes another, so a
/// reader that assembles both without coordination can pair a new event log
/// with an old envelope and report that mismatch as corruption.
///
/// Three properties keep this usable from read-only surfaces:
///
/// - The parent directory is never created. A read of something that does not
///   exist must not materialize it, and a missing parent also means no writer
///   can be holding anything inside it, so `op` runs directly.
/// - Acquisition is best effort. A store on a read-only mount, or any
///   filesystem that refuses the lock file, still serves the read unlocked
///   rather than failing it — the same exposure as before this lock existed.
///   Active contention waits up to the configured deadline and returns a typed
///   timeout instead of reading through a live writer.
/// - Re-entrancy is shared with the exclusive variant, so a read nested inside
///   a writer's own critical section runs directly instead of deadlocking on a
///   second descriptor.
///
/// Do not take the *write* lock for a target inside a read lock on that same
/// target. A nested request never upgrades the outer acquisition, so the
/// mutation would run under a shared lock that concurrent readers also hold.
pub fn with_shared_file_lock<T, E, F>(target_path: &Path, label: &str, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    with_shared_file_lock_options(target_path, label, FileLockOptions::default(), op)
}

/// [`with_shared_file_lock`] with an explicit, testable acquisition policy.
pub fn with_shared_file_lock_options<T, E, F>(
    target_path: &Path,
    label: &str,
    options: FileLockOptions,
    op: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let Some(parent) = target_path.parent() else {
        return op();
    };
    if !parent.is_dir() {
        return op();
    }
    let lock_path = resolved_lock_path(target_path)?;
    let Some(_held) = claim_lock_path(&lock_path) else {
        return op();
    };
    let _lock_file = match acquire_shared_file_lock(&lock_path, label, options) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(E::from(error)),
        Err(error) => {
            crate::tracing::debug!(
                target: "orbit.common.fs",
                lock_path = %lock_path.display(),
                label,
                error = %error,
                "shared lock unavailable; reading without writer coordination",
            );
            None
        }
    };

    op()
}

/// The lock path to open and to key re-entrancy on, resolved through symlinks
/// where the parent directory already exists.
///
/// Orbit reaches one task bundle by more than one route — the canonical store
/// path and the checkout projection that links to it — so keying re-entrancy
/// on the literal path would let a nested call miss its own outer lock and
/// deadlock on a second descriptor to the same file. Resolving the parent
/// collapses those routes to one key. An unresolvable parent means the
/// directory does not exist yet, so nothing can be holding a lock inside it.
fn resolved_lock_path(target_path: &Path) -> io::Result<PathBuf> {
    let lock_path = lock_path_for(target_path)?;
    let Some(file_name) = lock_path.file_name() else {
        return Ok(lock_path);
    };
    match lock_path.parent().map(fs::canonicalize) {
        Some(Ok(parent)) => Ok(parent.join(file_name)),
        _ => Ok(lock_path),
    }
}

fn lock_path_for(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path '{}' has no file name", path.display()),
        )
    })?;
    Ok(path.with_file_name(format!(".{file_name}.lock")))
}

pub(crate) fn open_private_file(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    let path = validated_private_file_path(path)?;
    apply_private_file_mode(options);
    apply_no_follow_final_component(options);
    let file = options.open(&path)?;
    set_private_file_permissions_for_open_file(&file)?;
    Ok(file)
}

/// Open `path` for a read-only inspection without following its final component.
///
/// On Unix the open is nonblocking as well as no-follow, so a FIFO swapped in
/// after a caller's pathname check cannot indefinitely block the reader. The
/// caller remains responsible for checking the opened descriptor's type and
/// mapping errors into its domain-specific behavior.
pub fn open_read_only_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    apply_read_only_no_follow(&mut options);
    options.open(path)
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

fn set_private_file_permissions_for_open_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

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
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

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
                let mut builder = fs::DirBuilder::new();
                builder.mode(PRIVATE_DIR_MODE);
                match builder.create(&current) {
                    Ok(()) => {
                        fs::set_permissions(
                            &current,
                            fs::Permissions::from_mode(PRIVATE_DIR_MODE),
                        )?;
                    }
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

#[cfg(all(unix, feature = "sqlite"))]
pub(crate) fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

#[cfg(all(not(unix), feature = "sqlite"))]
pub(crate) fn set_private_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
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

pub(crate) fn classify_lock_io(path: &Path, err: io::Error) -> io::Error {
    match write_access_error_message(path, &err) {
        Some(message) => io::Error::new(err.kind(), message),
        None => err,
    }
}

pub(crate) fn classify_or_wrap_lock_io(
    path: &Path,
    err: io::Error,
    fallback: impl FnOnce(&io::Error) -> String,
) -> io::Error {
    match write_access_error_message(path, &err) {
        Some(message) => io::Error::new(err.kind(), message),
        None => io::Error::other(fallback(&err)),
    }
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
