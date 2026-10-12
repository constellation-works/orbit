use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{create_new_private_file, create_private_dir_all, open_private_file};

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// Create `path` with private permissions and write `content` to it, failing
/// with [`io::ErrorKind::AlreadyExists`] when it is already there.
///
/// The exclusive create is the point: a caller that must not overwrite an
/// existing file gets that guarantee from the kernel rather than from a
/// `path.exists()` check something can win a race against. On Unix the file is
/// `0o600` whatever the ambient umask is, and parent directories this call
/// creates are `0o700`.
pub fn write_new_private_text(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    let mut file = create_new_private_file(path)?;
    file.write_all(content.as_bytes())?;
    sync_file(&file)
}

/// Create or replace the marker file `path`, holding `content`, and sync only
/// the directory entry that names it.
///
/// For a file whose presence is the fact it records: once this returns, the
/// file survives a crash, though `content` may not. One durability sync
/// where [`atomic_write_text`] takes two, the staged file's and its parent's.
pub fn write_presence_marker(path: &Path, content: &str) -> io::Result<()> {
    let target = validated_atomic_target(path)?;
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("validated marker target is missing its parent"))?;
    let mut options = fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    let mut file = open_private_file(&target, &mut options)?;
    file.write_all(content.as_bytes())?;
    drop(file);
    sync_parent_dir(&File::open(parent)?)
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
            sync_file(&file)?;
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
    sync_file(parent_dir)
}

// ---------------------------------------------------------------------------
// Durability syncs
// ---------------------------------------------------------------------------

/// Set to `1` to make durability syncs no-ops in a debug build's test run.
///
/// `.cargo/config.toml` sets it for every process Cargo launches, so test
/// binaries and the `orbit` children they spawn inherit it. On macOS each
/// sync is an `F_FULLFSYNC` that flushes the whole drive's write cache, and
/// concurrent suites queue behind each other's flushes. A test process loses
/// nothing by skipping them: only a power loss or kernel crash can observe a
/// missing sync. Release builds ignore the variable.
pub const RELAXED_DURABILITY_ENV: &str = "ORBIT_TEST_RELAXED_DURABILITY";

static RELAXED_DURABILITY: OnceLock<bool> = OnceLock::new();

/// Live [`crate::test_env::require_durable_syncs`] guards. Any one of them
/// restores real syncs process-wide.
pub(crate) static DURABLE_SYNC_HOLDS: AtomicUsize = AtomicUsize::new(0);

/// Whether a [`RELAXED_DURABILITY_ENV`] value relaxes durability in a build
/// that does (`debug_build`) or does not carry debug assertions.
///
/// Only an exact `1` in a debug build relaxes, so a release or installed
/// binary always syncs whatever its environment says.
#[doc(hidden)]
pub fn relaxed_durability_requested(value: Option<&OsStr>, debug_build: bool) -> bool {
    debug_build && value == Some(OsStr::new("1"))
}

/// Whether durability syncs are currently skipped. The variable is read once.
pub fn durability_syncs_relaxed() -> bool {
    let relaxed = *RELAXED_DURABILITY.get_or_init(|| {
        relaxed_durability_requested(
            std::env::var_os(RELAXED_DURABILITY_ENV).as_deref(),
            cfg!(debug_assertions),
        )
    });
    relaxed && DURABLE_SYNC_HOLDS.load(Ordering::SeqCst) == 0
}

thread_local! {
    /// Durability syncs requested on this thread, relaxed or not.
    static DURABILITY_SYNCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many durability syncs ([`sync_file`], [`sync_file_data`]) this thread
/// has requested, counting those a relaxed test run skips. A caller compares
/// two readings to bound the syncs a section of its own code performs.
pub fn durability_syncs_requested() -> u64 {
    DURABILITY_SYNCS.with(std::cell::Cell::get)
}

fn count_durability_sync() {
    DURABILITY_SYNCS.with(|syncs| syncs.set(syncs.get() + 1));
}

/// Flush `file`'s data and metadata to stable storage (`File::sync_all`).
///
/// Every durability sync in Orbit goes through here or [`sync_file_data`];
/// clippy's `disallowed-methods` rejects direct calls in production code. In a
/// relaxed test run ([`RELAXED_DURABILITY_ENV`]) it returns `Ok` without
/// syncing.
#[allow(
    clippy::disallowed_methods,
    reason = "the single durability-sync site; callers route through it so test runs can relax it"
)]
pub fn sync_file(file: &File) -> io::Result<()> {
    count_durability_sync();
    if durability_syncs_relaxed() {
        return Ok(());
    }
    file.sync_all()
}

/// Flush `file`'s data to stable storage (`File::sync_data`), relaxed in test
/// runs like [`sync_file`].
#[allow(
    clippy::disallowed_methods,
    reason = "the single durability-sync site; callers route through it so test runs can relax it"
)]
pub fn sync_file_data(file: &File) -> io::Result<()> {
    count_durability_sync();
    if durability_syncs_relaxed() {
        return Ok(());
    }
    file.sync_data()
}

/// The SQLite `synchronous` level to set where a store wants `durable`
/// (`"NORMAL"` or `"FULL"`): `"OFF"` in a relaxed test run, else `durable`.
///
/// Every `synchronous` pragma takes its level from here;
/// `scripts/check-durability-syncs.py` rejects one that does not.
pub fn sqlite_synchronous(durable: &'static str) -> &'static str {
    if durability_syncs_relaxed() {
        "OFF"
    } else {
        durable
    }
}

// ---------------------------------------------------------------------------
// Filesystem helpers beyond atomic write
// ---------------------------------------------------------------------------
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
#[allow(
    clippy::disallowed_methods,
    reason = "this compatibility helper deliberately promises non-atomic writes; durable replacements use atomic_write_text"
)]
pub fn write_text_with_parent(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    fs::write(path, content)
}
