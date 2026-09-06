//! Auto-task scheduler cursor state [ORB-10149]: per-definition last-fired
//! bookkeeping, host-local and workspace-scoped.
//!
//! Cursors live in `<orbit_dir>/state/auto-tasks.json` — workspace-local,
//! gitignored runtime state.
//! The git-versioned definition YAML is never rewritten by a scheduler fire,
//! so the store stays churn-free and a definition edit never races the
//! scheduler. Updates take an exclusive file lock and re-read under the lock,
//! so a manual run racing the routine merges instead of clobbering.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use orbit_common::OrbitError;
use orbit_types::workflow::{AutoTaskCursor, AutoTaskCursorState};

/// Path of the cursor state file under a workspace state dir.
pub fn cursor_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auto-tasks.json")
}

/// Read the current cursor state (missing or unparsable file → empty state;
/// every definition is then treated as never-observed, which only re-baselines
/// it — safe in both directions).
pub fn load_cursor_state(path: &Path) -> AutoTaskCursorState {
    fs::read_to_string(path)
        .ok()
        .map(|raw| parse_state(&raw))
        .unwrap_or_default()
}

pub(crate) fn parse_state(raw: &str) -> AutoTaskCursorState {
    serde_json::from_str(raw.trim()).unwrap_or_default()
}

/// Upsert one definition's cursor under an exclusive file lock,
/// read-modify-writing the file so other definitions' cursors survive.
pub fn upsert_cursor(path: &Path, name: &str, cursor: AutoTaskCursor) -> Result<(), OrbitError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Io(format!(
                "create auto-tasks state dir {}: {error}",
                parent.display()
            ))
        })?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| {
            OrbitError::Io(format!(
                "open auto-tasks state file {}: {error}",
                path.display()
            ))
        })?;
    file.lock_exclusive().map_err(|error| {
        OrbitError::Io(format!(
            "lock auto-tasks state file {}: {error}",
            path.display()
        ))
    })?;

    let result = write_locked(&mut file, path, name, cursor);
    let unlock = fs2::FileExt::unlock(&file).map_err(|error| {
        OrbitError::Io(format!(
            "unlock auto-tasks state file {}: {error}",
            path.display()
        ))
    });
    result.and(unlock)
}

fn write_locked(
    file: &mut File,
    path: &Path,
    name: &str,
    cursor: AutoTaskCursor,
) -> Result<(), OrbitError> {
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let mut state = parse_state(&raw);
    state.definitions.insert(name.to_string(), cursor);

    let encoded = serde_json::to_string_pretty(&state)
        .map_err(|error| OrbitError::Io(format!("encode auto-tasks state: {error}")))?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| file.write_all(encoded.as_bytes()))
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}
