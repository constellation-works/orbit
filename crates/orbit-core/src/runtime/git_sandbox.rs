//! Host-owned Git write boundaries for Linux provider namespaces.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

/// Protect the pointer entry as well as the real per-worktree and shared
/// metadata. Recovery payloads and refs live beneath the shared Git directory.
/// A symlink in the metadata path cannot be pinned by a bind of its target;
/// reject that layout rather than leave a replaceable pointer in a write root.
pub(crate) fn append_linux_git_denies(
    cwd: &Path,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), OrbitError> {
    let cwd = cwd.canonicalize().map_err(|error| {
        OrbitError::PolicyDenied(format!("resolve Git protection root: {error}"))
    })?;
    let Some(pointer) = find_git_pointer(&cwd)? else {
        // Orbit also supports workspaces without Git.
        return Ok(());
    };
    let pointer = protect_metadata_path(&pointer, resolved)?;
    let git_dir = if pointer.is_dir() {
        pointer
    } else {
        let text = read_metadata_pointer(&pointer)?;
        let target = text
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .ok_or_else(|| OrbitError::PolicyDenied("invalid Git metadata pointer".to_string()))?;
        let parent = pointer
            .parent()
            .ok_or_else(|| OrbitError::PolicyDenied("Git pointer has no parent".to_string()))?;
        protect_metadata_path(&parent.join(target), resolved)?
    };
    let mut metadata_root = git_dir.clone();
    let common_pointer = git_dir.join("commondir");
    if metadata_exists(&common_pointer)? {
        protect_metadata_path(&common_pointer, resolved)?;
        let target = read_metadata_pointer(&common_pointer)?;
        if target.trim().is_empty() {
            return Err(OrbitError::PolicyDenied(
                "empty Git commondir pointer".to_string(),
            ));
        }
        metadata_root = protect_metadata_path(&git_dir.join(target.trim()), resolved)?;
    }
    validate_linux_git_tree(&metadata_root)?;
    if !git_dir.starts_with(&metadata_root) {
        validate_linux_git_tree(&git_dir)?;
    }
    Ok(())
}

fn find_git_pointer(cwd: &Path) -> Result<Option<PathBuf>, OrbitError> {
    for root in cwd.ancestors() {
        let pointer = root.join(".git");
        if metadata_exists(&pointer)? {
            return Ok(Some(pointer));
        }
    }
    Ok(None)
}

fn metadata_exists(path: &Path) -> Result<bool, OrbitError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(metadata_error(path, error)),
    }
}

fn read_metadata_pointer(path: &Path) -> Result<String, OrbitError> {
    fs::read_to_string(path).map_err(|error| metadata_error(path, error))
}

fn protect_metadata_path(
    path: &Path,
    resolved: &mut ResolvedFsProfile,
) -> Result<PathBuf, OrbitError> {
    for ancestor in path.ancestors() {
        let metadata =
            fs::symlink_metadata(ancestor).map_err(|error| metadata_error(ancestor, error))?;
        if metadata.file_type().is_symlink() {
            return Err(OrbitError::PolicyDenied(format!(
                "Linux Git protection refuses symlink metadata path `{}`",
                ancestor.display()
            )));
        }
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| metadata_error(path, error))?;
    let metadata = fs::metadata(&canonical).map_err(|error| metadata_error(path, error))?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(OrbitError::PolicyDenied(
            "Linux Git protection refuses a special-file metadata pointer".to_string(),
        ));
    }
    if metadata.is_file() && metadata.nlink() > 1 {
        return Err(OrbitError::PolicyDenied(
            "Linux Git protection refuses hard-linked metadata pointer".to_string(),
        ));
    }
    let suffix = if metadata.is_dir() { "/**" } else { "" };
    resolved
        .modify
        .push(format!("!{}{suffix}", canonical.display()));
    Ok(canonical)
}

fn validate_linux_git_tree(root: &Path) -> Result<(), OrbitError> {
    for entry in fs::read_dir(root).map_err(|error| metadata_error(root, error))? {
        let entry = entry.map_err(|error| metadata_error(root, error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| metadata_error(&path, error))?;
        if (!metadata.is_file() && !metadata.is_dir())
            || (metadata.is_file() && metadata.nlink() > 1)
        {
            return Err(OrbitError::PolicyDenied(format!(
                "Linux Git protection refuses symlink, special-file or hard-linked metadata entry `{}`",
                path.display()
            )));
        }
        if metadata.is_dir() {
            validate_linux_git_tree(&path)?;
        }
    }
    Ok(())
}

fn metadata_error(path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "inspect protected Git metadata `{}`: {error}",
        path.display()
    ))
}

#[cfg(test)]
#[path = "tests/git_sandbox.rs"]
mod tests;
