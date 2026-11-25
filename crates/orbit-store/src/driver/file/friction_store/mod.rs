//! Locate legacy Markdown friction files for the one-shot import.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

/// Directory layout for read-only legacy Markdown friction records.
pub(crate) struct LegacyFrictionDir {
    pub(crate) root: PathBuf,
}

/// Resolve the legacy corpus root before enumerating records.
///
/// A workspace-local legacy tree is Orbit's own configuration, not
/// attacker-controlled input, and an ordinary layout may reach it through a
/// symlinked ancestor (a symlinked `$HOME`, a relocated data dir, macOS's
/// `/var` -> `/private/var`) or a symlinked root itself. Canonicalizing and
/// accepting any directory the root resolves to — rather than rejecting a
/// path that resolves through a symlink — matches the policy already applied
/// to `canonical_pipeline_worker_log_parent` and
/// `validated_linux_provider_state_root`. Containment against a corpus entry
/// that escapes this root is enforced separately, by `friction_record_paths`
/// refusing to follow any symlinked month or record entry during the walk.
fn validated_friction_root(frictions_root: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_root = fs::canonicalize(frictions_root).map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize friction corpus root {}: {error}",
            frictions_root.display()
        ))
    })?;

    if !canonical_root.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "friction corpus root must be a directory: {}",
            canonical_root.display()
        )));
    }

    Ok(canonical_root)
}

pub(crate) fn friction_record_paths(frictions_root: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let frictions_root = validated_friction_root(frictions_root)?;
    let mut paths = Vec::new();
    for month_entry in fs::read_dir(&frictions_root)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", frictions_root.display())))?
    {
        let month_entry = month_entry.map_err(|error| OrbitError::Io(error.to_string()))?;
        // `DirEntry::file_type` reports the entry itself (it does not follow a
        // symlink), so a month or record entry that is a symlink is neither
        // `is_dir()` nor `is_file()` here and is skipped below — including one
        // that resolves outside `frictions_root`.
        let month_type = month_entry
            .file_type()
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        if !month_type.is_dir() {
            continue;
        }
        let month_path = month_entry.path();
        for record_entry in fs::read_dir(&month_path)
            .map_err(|error| OrbitError::Io(format!("read {}: {error}", month_path.display())))?
        {
            let record_entry = record_entry.map_err(|error| OrbitError::Io(error.to_string()))?;
            let record_type = record_entry
                .file_type()
                .map_err(|error| OrbitError::Io(error.to_string()))?;
            let path = record_entry.path();
            if record_type.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("md")
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}
