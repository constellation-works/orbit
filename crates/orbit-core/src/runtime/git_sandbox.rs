//! Host-owned Git write boundaries for Linux provider namespaces.

use std::collections::HashMap;
use std::fs;
use std::io;
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
    // Git removes transient locks while host-side preparation is scanning.
    // Restart the entire tree rather than skip a missing entry: its replacement
    // and previously inspected siblings must still pass the same validation.
    // Keep directory identities across attempts so a retry cannot bless a
    // redirected root or a replaced directory encountered on the first pass.
    const MAX_ATTEMPTS: usize = 3;
    let mut directories = HashMap::new();
    let mut attempts = 0;
    loop {
        attempts += 1;
        validate_git_tree_ancestors(root, &mut directories)?;
        let result = scan_linux_git_tree(root, &mut directories);
        validate_git_tree_ancestors(root, &mut directories)?;
        match result {
            Ok(()) => return Ok(()),
            Err(GitTreeScanError::Denied(error)) => return Err(error),
            Err(GitTreeScanError::Disappeared { path, error }) => {
                if attempts == MAX_ATTEMPTS {
                    return Err(OrbitError::PolicyDenied(format!(
                        "protected Git metadata remained unstable after {MAX_ATTEMPTS} scans: {}",
                        metadata_error(&path, error)
                    )));
                }
            }
        }
    }
}

type DirectoryIdentities = HashMap<PathBuf, (u64, u64)>;

fn validate_git_tree_ancestors(
    root: &Path,
    directories: &mut DirectoryIdentities,
) -> Result<(), OrbitError> {
    // The root and its ancestors must remain present, unaliased directories.
    // Only disappearing descendants are eligible for revalidation.
    for ancestor in root.ancestors() {
        let metadata =
            fs::symlink_metadata(ancestor).map_err(|error| metadata_error(ancestor, error))?;
        validate_git_directory(ancestor, &metadata, directories)?;
    }
    Ok(())
}

fn validate_git_directory(
    path: &Path,
    metadata: &fs::Metadata,
    directories: &mut DirectoryIdentities,
) -> Result<(), OrbitError> {
    if !metadata.is_dir() {
        return Err(OrbitError::PolicyDenied(format!(
            "Linux Git protection refuses non-directory metadata traversal `{}`",
            path.display()
        )));
    }
    let identity = (metadata.dev(), metadata.ino());
    if let Some(previous) = directories.insert(path.to_path_buf(), identity)
        && previous != identity
    {
        return Err(OrbitError::PolicyDenied(format!(
            "Linux Git protection refuses replaced metadata directory `{}`",
            path.display()
        )));
    }
    Ok(())
}

enum GitTreeScanError {
    Disappeared { path: PathBuf, error: io::Error },
    Denied(OrbitError),
}

impl From<OrbitError> for GitTreeScanError {
    fn from(error: OrbitError) -> Self {
        Self::Denied(error)
    }
}

fn scan_error(path: &Path, error: io::Error) -> GitTreeScanError {
    if error.kind() == io::ErrorKind::NotFound {
        GitTreeScanError::Disappeared {
            path: path.to_path_buf(),
            error,
        }
    } else {
        GitTreeScanError::Denied(metadata_error(path, error))
    }
}

fn scan_linux_git_tree(
    root: &Path,
    directories: &mut DirectoryIdentities,
) -> Result<(), GitTreeScanError> {
    #[cfg(test)]
    run_scan_hook(GitScanStage::ReadDirectory, root).map_err(|error| scan_error(root, error))?;
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory(root, &metadata, directories)?;
    for entry in fs::read_dir(root).map_err(|error| scan_error(root, error))? {
        let entry = entry.map_err(|error| scan_error(root, error))?;
        let path = entry.path();
        #[cfg(test)]
        run_scan_hook(GitScanStage::InspectEntry, &path)
            .map_err(|error| scan_error(&path, error))?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| scan_error(&path, error))?;
        if (!metadata.is_file() && !metadata.is_dir())
            || (metadata.is_file() && metadata.nlink() > 1)
        {
            return Err(OrbitError::PolicyDenied(format!(
                "Linux Git protection refuses symlink, special-file or hard-linked metadata entry `{}`",
                path.display()
            )).into());
        }
        if metadata.is_dir() {
            validate_git_directory(&path, &metadata, directories)?;
            scan_linux_git_tree(&path, directories)?;
        }
    }
    // read_dir may have opened an older directory after a concurrent rename.
    // Never accept its contents as validation of the replacement at this path.
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory(root, &metadata, directories)?;
    Ok(())
}

fn metadata_error(path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "inspect protected Git metadata `{}`: {error}",
        path.display()
    ))
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitScanStage {
    ReadDirectory,
    InspectEntry,
}

#[cfg(test)]
type GitScanHook = Box<dyn FnMut(GitScanStage, &Path) -> io::Result<()>>;

#[cfg(test)]
thread_local! {
    static SCAN_HOOK: std::cell::RefCell<Option<GitScanHook>> = const { std::cell::RefCell::new(None) };
}

/// Per-thread interleaving seam; production always uses the real filesystem.
#[cfg(test)]
pub(crate) struct GitScanHookGuard(Option<GitScanHook>);

#[cfg(test)]
impl GitScanHookGuard {
    pub(crate) fn install(
        hook: impl FnMut(GitScanStage, &Path) -> io::Result<()> + 'static,
    ) -> Self {
        Self(SCAN_HOOK.with(|slot| slot.replace(Some(Box::new(hook)))))
    }
}

#[cfg(test)]
impl Drop for GitScanHookGuard {
    fn drop(&mut self) {
        SCAN_HOOK.with(|slot| slot.replace(self.0.take()));
    }
}

#[cfg(test)]
fn run_scan_hook(stage: GitScanStage, path: &Path) -> io::Result<()> {
    SCAN_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}
