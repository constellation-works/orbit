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
//! unchanged for investigation.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
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
    let validated = match validated_cursor_state_path(path)? {
        Some(validated) => validated,
        None => return Ok(AutoTaskCursorState::default()),
    };

    match fs::read_to_string(&validated) {
        Ok(raw) => parse_state(&raw, &validated),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(AutoTaskCursorState::default())
        }
        Err(error) => Err(OrbitError::Io(format!(
            "unreadable auto-task cursor state {}: {error}; file left unchanged for investigation",
            validated.display()
        ))),
    }
}

/// Resolve `path` through its canonical parent dir, rejecting a symlinked
/// target. `path` is built by [`cursor_state_path`] from a caller-selected
/// state dir (workspace discovery, `--root`, or web dashboard workspace
/// routing), so canonicalizing the parent and refusing to follow a symlinked
/// result keeps the read inside the selected state dir instead of a planted
/// symlink's target [ORB-11948]. `Ok(None)` means "no existing file", which
/// callers treat as empty baseline state, matching prior behavior for a
/// missing state dir or a missing data file.
fn validated_cursor_state_path(path: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(parent) = path.parent() else {
        return Ok(Some(path.to_path_buf()));
    };
    let Some(file_name) = path.file_name() else {
        return Ok(Some(path.to_path_buf()));
    };

    let canonical_parent = match parent.canonicalize() {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize auto-task state dir {}: {error}",
                parent.display()
            )));
        }
    };
    let candidate = canonical_parent.join(file_name);

    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(OrbitError::InvalidInput(format!(
                "auto-task cursor state path must not be a symlink: {}",
                candidate.display()
            )))
        }
        Ok(_) => Ok(Some(candidate)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect auto-task cursor state path {}: {error}",
            candidate.display()
        ))),
    }
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
