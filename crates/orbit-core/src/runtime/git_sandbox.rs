//! Host-owned Git write boundaries for macOS and Linux provider sandboxes.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;

/// Protect the pointer entry as well as the real per-worktree and shared
/// metadata. Recovery payloads and refs live beneath the shared Git directory.
/// A symlink in the metadata path can leave a writable alias of its target;
/// reject that layout rather than leave a replaceable pointer in a write root.
pub(crate) fn append_git_denies(
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
    validate_git_tree(&metadata_root)?;
    if !git_dir.starts_with(&metadata_root) {
        validate_git_tree(&git_dir)?;
    }
    Ok(())
}

/// Run the host-side protection scan on a checkout exactly as sandbox
/// preparation does, without a profile to extend. Errors are the refusals a
/// leaf in that checkout would meet.
pub(crate) fn scan_checkout(cwd: &Path) -> Result<(), OrbitError> {
    let mut scratch = ResolvedFsProfile {
        name: "git-protection-scan".to_string(),
        read: Vec::new(),
        modify: Vec::new(),
    };
    append_git_denies(cwd, &mut scratch)
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
                "Git protection refuses symlink metadata path `{}`",
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
            "Git protection refuses a special-file metadata pointer".to_string(),
        ));
    }
    if metadata.is_file() && metadata.nlink() > 1 {
        return Err(OrbitError::PolicyDenied(
            "Git protection refuses hard-linked metadata pointer".to_string(),
        ));
    }
    let suffix = if metadata.is_dir() { "/**" } else { "" };
    resolved
        .modify
        .push(format!("!{}{suffix}", canonical.display()));
    Ok(canonical)
}

fn validate_git_tree(root: &Path) -> Result<(), OrbitError> {
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
        // Names are counted per attempt: a retry rescans every entry.
        let mut links = ObjectLinks::default();
        let result = scan_git_tree(root, ScanScope::Root, &mut directories, &mut links);
        validate_git_tree_ancestors(root, &mut directories)?;
        match result {
            Ok(()) => match links.first_escape() {
                None => return Ok(()),
                // Git may have unlinked a temporary name between its inode
                // being read and its sibling being counted; a name that is
                // still missing on the last attempt escapes the store.
                Some(escape) if attempts == MAX_ATTEMPTS => return Err(escape),
                Some(_) => {}
            },
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
            "Git protection refuses non-directory metadata traversal `{}`",
            path.display()
        )));
    }
    let identity = (metadata.dev(), metadata.ino());
    if let Some(previous) = directories.insert(path.to_path_buf(), identity)
        && previous != identity
    {
        return Err(OrbitError::PolicyDenied(format!(
            "Git protection refuses replaced metadata directory `{}`",
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

/// Where a directory sits in the protected tree. Only the object store
/// directly under the scan root tolerates hard links among its own names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanScope {
    Root,
    Metadata,
    ObjectStore,
}

/// Git writes an object to a temporary name, links it to the final name and
/// unlinks the temporary one. A write interrupted between the last two steps
/// leaves both names, so `objects/` may hold hard links that are Git's own.
/// They are accepted only when every name of the inode is inside the store.
#[derive(Default)]
struct ObjectLinks {
    inodes: HashMap<(u64, u64), ObjectInode>,
}

struct ObjectInode {
    nlink: u64,
    names_seen: u64,
    reported: PathBuf,
    reported_is_temp: bool,
}

impl ObjectLinks {
    fn record(&mut self, path: &Path, metadata: &fs::Metadata) {
        let temp = is_git_temp_name(path);
        let entry = self
            .inodes
            .entry((metadata.dev(), metadata.ino()))
            .or_insert_with(|| ObjectInode {
                nlink: metadata.nlink(),
                names_seen: 0,
                reported: path.to_path_buf(),
                reported_is_temp: temp,
            });
        entry.names_seen += 1;
        if temp && !entry.reported_is_temp {
            entry.reported = path.to_path_buf();
            entry.reported_is_temp = true;
        }
    }

    /// A name of some inode that lives outside the store, if any.
    fn first_escape(&self) -> Option<OrbitError> {
        self.inodes
            .values()
            .find(|inode| inode.names_seen < inode.nlink)
            .map(|inode| metadata_entry_refusal(&inode.reported, true))
    }
}

/// The names Git gives a loose object, pack or index until it is complete.
fn is_git_temp_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            ["tmp_obj_", "tmp_pack_", "tmp_idx_"]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
}

fn metadata_entry_refusal(path: &Path, hard_linked: bool) -> OrbitError {
    let remedy = if hard_linked && is_git_temp_name(path) {
        format!(
            "; it is a temporary file left by an interrupted Git write. Delete `{}` and run \
             `git fsck`",
            path.display()
        )
    } else {
        String::new()
    };
    OrbitError::PolicyDenied(format!(
        "Git protection refuses symlink, special-file or hard-linked metadata entry `{}`{remedy}",
        path.display()
    ))
}

fn scan_git_tree(
    root: &Path,
    scope: ScanScope,
    directories: &mut DirectoryIdentities,
    links: &mut ObjectLinks,
) -> Result<(), GitTreeScanError> {
    #[cfg(all(test, target_os = "linux"))]
    run_scan_hook(GitScanStage::ReadDirectory, root).map_err(|error| scan_error(root, error))?;
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory(root, &metadata, directories)?;
    for entry in fs::read_dir(root).map_err(|error| scan_error(root, error))? {
        let entry = entry.map_err(|error| scan_error(root, error))?;
        let path = entry.path();
        #[cfg(all(test, target_os = "linux"))]
        run_scan_hook(GitScanStage::InspectEntry, &path)
            .map_err(|error| scan_error(&path, error))?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| scan_error(&path, error))?;
        if !metadata.is_file() && !metadata.is_dir() {
            return Err(metadata_entry_refusal(&path, false).into());
        }
        if metadata.is_file() && metadata.nlink() > 1 {
            if scope == ScanScope::ObjectStore {
                links.record(&path, &metadata);
            } else {
                return Err(metadata_entry_refusal(&path, true).into());
            }
        }
        if metadata.is_dir() {
            validate_git_directory(&path, &metadata, directories)?;
            let child = match scope {
                ScanScope::Root if entry.file_name() == "objects" => ScanScope::ObjectStore,
                ScanScope::ObjectStore => ScanScope::ObjectStore,
                ScanScope::Root | ScanScope::Metadata => ScanScope::Metadata,
            };
            scan_git_tree(&path, child, directories, links)?;
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

#[cfg(all(test, target_os = "linux"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitScanStage {
    ReadDirectory,
    InspectEntry,
}

#[cfg(all(test, target_os = "linux"))]
type GitScanHook = Box<dyn FnMut(GitScanStage, &Path) -> io::Result<()>>;

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    static SCAN_HOOK: std::cell::RefCell<Option<GitScanHook>> = const { std::cell::RefCell::new(None) };
}

/// Per-thread interleaving seam; production always uses the real filesystem.
#[cfg(all(test, target_os = "linux"))]
pub(crate) struct GitScanHookGuard(Option<GitScanHook>);

#[cfg(all(test, target_os = "linux"))]
impl GitScanHookGuard {
    pub(crate) fn install(
        hook: impl FnMut(GitScanStage, &Path) -> io::Result<()> + 'static,
    ) -> Self {
        Self(SCAN_HOOK.with(|slot| slot.replace(Some(Box::new(hook)))))
    }
}

#[cfg(all(test, target_os = "linux"))]
impl Drop for GitScanHookGuard {
    fn drop(&mut self) {
        SCAN_HOOK.with(|slot| slot.replace(self.0.take()));
    }
}

#[cfg(all(test, target_os = "linux"))]
fn run_scan_hook(stage: GitScanStage, path: &Path) -> io::Result<()> {
    SCAN_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}
