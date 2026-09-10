//! Auto-task scheduler cursor state [ORB-10149]: per-definition last-fired
//! bookkeeping, host-local and workspace-scoped.
//!
//! Cursors live in `<orbit_dir>/state/auto-tasks.json` — workspace-local,
//! gitignored runtime state.
//! The git-versioned definition YAML is never rewritten by a scheduler fire,
//! so the store stays churn-free and a definition edit never races the
//! scheduler.
//!
//! Slot admission and persistence share one stable sidecar lock
//! (`.auto-tasks.json.lock`). The data file is replaced by rename, so the
//! lock is not the inode being replaced and readers never observe truncated
//! JSON. Missing state is an empty map (first-observation baseline). An
//! existing unreadable or malformed file is an explicit error and is left
//! unchanged for investigation. Loads read through a descriptor opened without
//! following the final component, so the path check and the read cannot be
//! separated by a swap [ORB-12026].

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_common::fs::open_read_only_no_follow;
use orbit_types::workflow::{AutoTaskCursor, AutoTaskCursorState};

#[cfg(test)]
use std::cell::RefCell;

/// Path of the cursor state file under a workspace state dir.
pub fn cursor_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auto-tasks.json")
}

/// Stable sidecar lock for [`cursor_state_path`]. Matches
/// `orbit_common::fs::io::with_exclusive_file_lock` so atomic replacement of
/// the data file cannot drop exclusion.
pub fn cursor_lock_path(state_path: &Path) -> PathBuf {
    let file_name = state_path.file_name().map_or_else(
        || "auto-tasks.json".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    state_path.with_file_name(format!(".{file_name}.lock"))
}

/// Read the current cursor state.
///
/// A missing file is empty state. An existing file that cannot be read or
/// parsed is an error; callers must not treat that as a baseline or rewrite it.
pub fn load_cursor_state(path: &Path) -> Result<AutoTaskCursorState, OrbitError> {
    load_cursor_state_with_hook(path, |_| Ok(()))
}

/// Load through an opened descriptor, letting a test perturb the file between
/// the pathname probe and the open.
#[cfg(test)]
pub(crate) fn load_cursor_state_after_check<F>(
    path: &Path,
    before_open: F,
) -> Result<AutoTaskCursorState, OrbitError>
where
    F: FnOnce(&Path) -> Result<(), OrbitError>,
{
    load_cursor_state_with_hook(path, before_open)
}

fn load_cursor_state_with_hook<F>(
    path: &Path,
    before_open: F,
) -> Result<AutoTaskCursorState, OrbitError>
where
    F: FnOnce(&Path) -> Result<(), OrbitError>,
{
    let Some((resolved, mut file)) = open_cursor_state_file(path, before_open)? else {
        return Ok(AutoTaskCursorState::default());
    };

    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| unreadable_cursor_state(&resolved, &error))?;

    parse_state(&raw, &resolved)
}

/// Final component of `path`, accepted only when the caller actually named a
/// child of some directory.
///
/// `Path::file_name` already yields `None` for a filesystem root, for `.`, and
/// for anything terminating in `..`, so those shapes fail closed here rather
/// than falling back to the unvalidated input. The contract deliberately stays
/// "one normal component" instead of the fixed `auto-tasks.json` that
/// [`cursor_state_path`] builds: [`load_cursor_state`] is public and callers
/// may point it at an alternate basename.
fn validated_cursor_state_file_name(path: &Path) -> Result<PathBuf, OrbitError> {
    path.file_name().map(PathBuf::from).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "auto-task cursor state path must name a file inside a state dir: {}",
            path.display()
        ))
    })
}

/// Canonical directory that owns the cursor-state file.
///
/// `path` is built from a caller-selected state dir (workspace discovery,
/// `--root`, or web dashboard workspace routing), so the parent is
/// canonicalized rather than rejected and supported aliases — a symlinked
/// state dir or checkout projection — keep resolving [ORB-11948]. This
/// directory is the authority every later probe and open is resolved against.
/// A bare relative filename is owned by the current directory, matching where
/// [`CursorSession::save`] would write it. `Ok(None)` means the directory does
/// not exist, which callers treat as empty baseline state.
fn validated_cursor_state_dir(path: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "auto-task cursor state path has no parent dir: {}",
            path.display()
        ))
    })?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };

    match parent.canonicalize() {
        Ok(dir) => Ok(Some(dir)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to canonicalize auto-task state dir {}: {error}",
            parent.display()
        ))),
    }
}

/// Open the existing cursor-state file beneath its canonical directory without
/// following a swapped final component.
///
/// Both the directory and the final component are validated before the first
/// metadata probe, so no sink here ever sees the raw caller path. Unix opens
/// with `O_NOFOLLOW | O_NONBLOCK` and Windows opens the reparse point itself.
/// The resulting descriptor is re-checked for a regular file, so a symlink planted between
/// the probe and the open fails instead of redirecting the read. A platform
/// with neither primitive still performs the pathname and descriptor checks
/// but cannot close that final-component race. Because ancestors are
/// canonicalized rather than rejected, this leaf protection does not claim to
/// stop a concurrent rename or replacement of a mutable ancestor directory.
///
/// `Ok(None)` means "no existing file", matching prior behavior for a missing
/// state dir or a missing data file.
fn open_cursor_state_file<F>(
    path: &Path,
    before_open: F,
) -> Result<Option<(PathBuf, File)>, OrbitError>
where
    F: FnOnce(&Path) -> Result<(), OrbitError>,
{
    let file_name = validated_cursor_state_file_name(path)?;
    let Some(canonical_dir) = validated_cursor_state_dir(path)? else {
        return Ok(None);
    };
    let candidate = canonical_dir.join(file_name);

    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(symlinked_cursor_state(&candidate));
        }
        Ok(metadata) if !metadata.is_file() => return Err(irregular_cursor_state(&candidate)),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to inspect auto-task cursor state path {}: {error}",
                candidate.display()
            )));
        }
    }

    before_open(&candidate)?;

    let file = match open_read_only_no_follow(&candidate) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_open_refusal(&error) => {
            return Err(symlinked_cursor_state(&candidate));
        }
        Err(error) => return Err(unreadable_cursor_state(&candidate, &error)),
    };

    let metadata = file.metadata().map_err(|error| {
        OrbitError::Io(format!(
            "failed to inspect auto-task cursor state path {}: {error}",
            candidate.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(irregular_cursor_state(&candidate));
    }

    Ok(Some((candidate, file)))
}

fn symlinked_cursor_state(path: &Path) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "auto-task cursor state path must not be a symlink: {}",
        path.display()
    ))
}

fn irregular_cursor_state(path: &Path) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "auto-task cursor state path must be a regular file: {}",
        path.display()
    ))
}

fn unreadable_cursor_state(path: &Path, error: &std::io::Error) -> OrbitError {
    OrbitError::Io(format!(
        "unreadable auto-task cursor state {}: {error}; file left unchanged for investigation",
        path.display()
    ))
}

/// `O_NOFOLLOW` reports a symlinked final component as `ELOOP`, which has no
/// stable [`std::io::ErrorKind`] to match on.
#[cfg(unix)]
fn is_symlink_open_refusal(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

/// Elsewhere a symlinked final component is caught by the descriptor's own
/// file-type check rather than by an open-time refusal.
#[cfg(not(unix))]
fn is_symlink_open_refusal(_error: &std::io::Error) -> bool {
    false
}

fn parse_state(raw: &str, path: &Path) -> Result<AutoTaskCursorState, OrbitError> {
    serde_json::from_str(raw.trim()).map_err(|error| {
        OrbitError::Store(format!(
            "malformed auto-task cursor state {}: {error}; file left unchanged for investigation",
            path.display()
        ))
    })
}

/// Hold the sidecar lock, load current state, and run `op`.
///
/// `op` may [`CursorSession::save`] more than once (claim, then checkpoint).
/// A successful `op` does not implicitly write; callers persist explicitly.
pub fn with_cursor_lock<T, F>(path: &Path, op: F) -> Result<T, OrbitError>
where
    F: FnOnce(&mut CursorSession) -> Result<T, OrbitError>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Io(format!(
                "create auto-tasks state dir {}: {error}",
                parent.display()
            ))
        })?;
    }

    with_exclusive_file_lock(path, "auto-task cursor", || {
        let state = load_cursor_state(path)?;
        let mut session = CursorSession {
            path: path.to_path_buf(),
            state,
        };
        op(&mut session)
    })
}

/// Locked cursor file session. Mutations are local until [`Self::save`].
pub struct CursorSession {
    path: PathBuf,
    /// Current in-memory cursor map, including other definitions' rows.
    pub state: AutoTaskCursorState,
}

impl CursorSession {
    /// Atomically replace the state file with `self.state`.
    ///
    /// The sidecar lock, not this inode, maintains exclusion. A write failure
    /// leaves the previous complete JSON in place.
    pub fn save(&self) -> Result<(), OrbitError> {
        fail_if_injected_save()?;
        let encoded = serde_json::to_string_pretty(&self.state)
            .map_err(|error| OrbitError::Io(format!("encode auto-tasks state: {error}")))?;
        atomic_write_text(&self.path, &encoded)
            .map_err(|error| OrbitError::from_write_io(&self.path, error))
    }
}

/// Upsert one definition's cursor under the sidecar lock,
/// read-modify-writing so other definitions' cursors survive.
pub fn upsert_cursor(path: &Path, name: &str, cursor: AutoTaskCursor) -> Result<(), OrbitError> {
    with_cursor_lock(path, |session| {
        session.state.definitions.insert(name.to_string(), cursor);
        session.save()
    })
}

#[cfg(test)]
thread_local! {
    static INJECTED_SAVE_FAULTS: RefCell<usize> = const { RefCell::new(0) };
}

#[cfg(test)]
pub(crate) fn inject_cursor_save_failures(count: usize) {
    INJECTED_SAVE_FAULTS.with(|cell| *cell.borrow_mut() = count);
}

fn fail_if_injected_save() -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_SAVE_FAULTS.with(|cell| {
            let mut remaining = cell.borrow_mut();
            if *remaining == 0 {
                return false;
            }
            *remaining -= 1;
            true
        });
        if hit {
            return Err(OrbitError::Store(
                "injected auto-task cursor save failure".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
