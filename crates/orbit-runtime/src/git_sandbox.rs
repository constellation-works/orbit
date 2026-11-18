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
pub fn append_git_denies(cwd: &Path, resolved: &mut ResolvedFsProfile) -> Result<(), OrbitError> {
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
pub fn scan_checkout(cwd: &Path) -> Result<(), OrbitError> {
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
    validate_git_directory_with_pinned(path, metadata, directories, None)
}

fn validate_git_directory_with_pinned(
    path: &Path,
    metadata: &fs::Metadata,
    directories: &mut DirectoryIdentities,
    pinned: Option<&DirectoryIdentities>,
) -> Result<(), OrbitError> {
    if !metadata.is_dir() {
        return Err(OrbitError::PolicyDenied(format!(
            "Git protection refuses non-directory metadata traversal `{}`",
            path.display()
        )));
    }
    let identity = (metadata.dev(), metadata.ino());
    if let Some(pinned) = pinned
        && let Some(previous) = pinned.get(path)
        && *previous != identity
    {
        return Err(OrbitError::PolicyDenied(format!(
            "Git protection refuses replaced metadata directory `{}`",
            path.display()
        )));
    }
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

    /// Add the names another scan of the same store counted, as if this scan
    /// had seen them next: counts add up and a temporary name is preferred.
    fn absorb(&mut self, other: ObjectLinks) {
        for (key, inode) in other.inodes {
            match self.inodes.entry(key) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(inode);
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let entry = slot.get_mut();
                    entry.names_seen += inode.names_seen;
                    if inode.reported_is_temp && !entry.reported_is_temp {
                        entry.reported = inode.reported;
                        entry.reported_is_temp = true;
                    }
                }
            }
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
    scan_git_tree_with_pinned(root, scope, directories, links, None)
}

fn scan_git_tree_with_pinned(
    root: &Path,
    scope: ScanScope,
    directories: &mut DirectoryIdentities,
    links: &mut ObjectLinks,
    pinned: Option<&DirectoryIdentities>,
) -> Result<(), GitTreeScanError> {
    #[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
    run_scan_hook(GitScanStage::ReadDirectory, root).map_err(|error| scan_error(root, error))?;
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory_with_pinned(root, &metadata, directories, pinned)?;
    for entry in fs::read_dir(root).map_err(|error| scan_error(root, error))? {
        let entry = entry.map_err(|error| scan_error(root, error))?;
        let path = entry.path();
        #[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
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
            validate_git_directory_with_pinned(&path, &metadata, directories, pinned)?;
            match scope {
                ScanScope::Root if entry.file_name() == "objects" => {
                    scan_object_store(&path, directories, links)?;
                }
                ScanScope::ObjectStore => {
                    scan_git_tree_with_pinned(
                        &path,
                        ScanScope::ObjectStore,
                        directories,
                        links,
                        pinned,
                    )?;
                }
                ScanScope::Root | ScanScope::Metadata => {
                    scan_git_tree_with_pinned(
                        &path,
                        ScanScope::Metadata,
                        directories,
                        links,
                        pinned,
                    )?;
                }
            }
        }
    }
    // read_dir may have opened an older directory after a concurrent rename.
    // Never accept its contents as validation of the replacement at this path.
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory_with_pinned(root, &metadata, directories, pinned)?;
    Ok(())
}

/// Most threads one object-store scan uses.
const MAX_OBJECT_SCAN_THREADS: usize = 8;

/// One entry directly under `objects/`, in `read_dir` order.
enum ObjectStoreEntry {
    /// A file with several names; recorded in the store's link counts.
    Linked(PathBuf, fs::Metadata),
    /// A fan-out, `pack` or `info` directory, scanned as its own subtree.
    Directory(PathBuf, fs::Metadata),
    /// The scan stops here, as the sequential walk would.
    Failed(GitTreeScanError),
}

/// What one subtree scan found: its outcome, the directory identities it
/// newly pinned (not already in the shared set) and the object names it counted.
struct SubtreeScan {
    result: Result<(), GitTreeScanError>,
    directories: DirectoryIdentities,
    links: ObjectLinks,
}

/// Scan `objects/` as [`scan_git_tree`] would, with its subdirectories (the
/// loose-object fan-out holds nearly every metadata entry) spread over a few
/// threads. Each subtree runs the same sequential scan against the identities
/// pinned so far; the results are folded back in `read_dir` order, so the
/// first refusal, the pinned identities and the link counts are the ones the
/// sequential walk would have produced.
fn scan_object_store(
    root: &Path,
    directories: &mut DirectoryIdentities,
    links: &mut ObjectLinks,
) -> Result<(), GitTreeScanError> {
    // The interleaving seam is per thread; keep its scans on this one.
    #[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
    if scan_hook_installed() {
        return scan_git_tree(root, ScanScope::ObjectStore, directories, links);
    }
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory(root, &metadata, directories)?;
    let entries = list_object_store(root);
    let subtrees = entries
        .iter()
        .filter_map(|entry| match entry {
            ObjectStoreEntry::Directory(path, metadata) => Some((path.as_path(), metadata)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut scans = scan_subtrees(&subtrees, directories).into_iter();
    for entry in entries {
        match entry {
            ObjectStoreEntry::Linked(path, metadata) => links.record(&path, &metadata),
            ObjectStoreEntry::Directory(..) => {
                let Some(scan) = scans.next() else {
                    return Err(GitTreeScanError::Denied(OrbitError::PolicyDenied(format!(
                        "Git protection lost a subtree scan under `{}`",
                        root.display()
                    ))));
                };
                directories.extend(scan.directories);
                links.absorb(scan.links);
                scan.result?;
            }
            ObjectStoreEntry::Failed(error) => return Err(error),
        }
    }
    // read_dir may have opened an older directory after a concurrent rename.
    // Never accept its contents as validation of the replacement at this path.
    let metadata = fs::symlink_metadata(root).map_err(|error| scan_error(root, error))?;
    validate_git_directory(root, &metadata, directories)?;
    Ok(())
}

/// Inspect the entries directly under `objects/`, stopping at the first one
/// the scan refuses, exactly where the sequential walk would stop.
fn list_object_store(root: &Path) -> Vec<ObjectStoreEntry> {
    let mut entries = Vec::new();
    let listing = match fs::read_dir(root) {
        Ok(listing) => listing,
        Err(error) => {
            entries.push(ObjectStoreEntry::Failed(scan_error(root, error)));
            return entries;
        }
    };
    for entry in listing {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                entries.push(ObjectStoreEntry::Failed(scan_error(root, error)));
                break;
            }
        };
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                entries.push(ObjectStoreEntry::Failed(scan_error(&path, error)));
                break;
            }
        };
        if metadata.is_dir() {
            entries.push(ObjectStoreEntry::Directory(path, metadata));
        } else if !metadata.is_file() {
            entries.push(ObjectStoreEntry::Failed(
                metadata_entry_refusal(&path, false).into(),
            ));
            break;
        } else if metadata.nlink() > 1 {
            entries.push(ObjectStoreEntry::Linked(path, metadata));
        }
    }
    entries
}

/// Scan each subtree against the shared identities pinned so far, on up
/// to [`MAX_OBJECT_SCAN_THREADS`] threads. Results come back in input order.
/// Each subtree collects only identities not already pinned, so retained
/// pins grow with the directories in that subtree, not with the shared set.
/// Once a subtree fails, later ones are skipped: the fold stops at that
/// failure and never reads them.
fn scan_subtrees(
    subtrees: &[(&Path, &fs::Metadata)],
    pinned: &DirectoryIdentities,
) -> Vec<SubtreeScan> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let scan_one = |path: &Path, metadata: &fs::Metadata| {
        let mut directories = DirectoryIdentities::new();
        let mut links = ObjectLinks::default();
        let result =
            validate_git_directory_with_pinned(path, metadata, &mut directories, Some(pinned))
                .map_err(GitTreeScanError::from)
                .and_then(|()| {
                    scan_git_tree_with_pinned(
                        path,
                        ScanScope::ObjectStore,
                        &mut directories,
                        &mut links,
                        Some(pinned),
                    )
                });
        directories.retain(|path, identity| pinned.get(path) != Some(identity));
        SubtreeScan {
            result,
            directories,
            links,
        }
    };
    let threads = scan_thread_count(subtrees.len());
    if threads <= 1 {
        let mut scans = Vec::with_capacity(subtrees.len());
        for (path, metadata) in subtrees {
            let scan = scan_one(path, metadata);
            let failed = scan.result.is_err();
            scans.push(scan);
            if failed {
                break;
            }
        }
        return scans;
    }
    let next = AtomicUsize::new(0);
    let first_failure = AtomicUsize::new(usize::MAX);
    let work = || {
        let mut done = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some((path, metadata)) = subtrees.get(index) else {
                return done;
            };
            if index > first_failure.load(Ordering::Relaxed) {
                continue;
            }
            let scan = scan_one(path, metadata);
            if scan.result.is_err() {
                first_failure.fetch_min(index, Ordering::Relaxed);
            }
            done.push((index, scan));
        }
    };
    let mut finished = std::thread::scope(|scope| {
        // The OS may refuse a thread; a worker that cannot be created leaves
        // its subtrees to the others. The calling thread works the same queue,
        // so every subtree is scanned even when no worker could start.
        let workers = (1..threads)
            .filter_map(|_| spawn_scan_worker(scope, work).ok())
            .collect::<Vec<_>>();
        let mut finished = work();
        for worker in workers {
            finished.extend(
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            );
        }
        finished
    });
    finished.sort_by_key(|(index, _)| *index);
    // Every index up to the first failure was scanned, so the prefix is
    // contiguous; anything past it is dropped.
    let cutoff = first_failure.load(Ordering::Relaxed);
    finished
        .into_iter()
        .take_while(|(index, _)| *index <= cutoff)
        .map(|(_, scan)| scan)
        .collect()
}

/// Threads to scan `subtrees` on, the calling thread included.
fn scan_thread_count(subtrees: usize) -> usize {
    #[cfg(any(test, feature = "test-support"))]
    if worker_spawn_refused_for_test() {
        return subtrees.min(MAX_OBJECT_SCAN_THREADS);
    }
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .clamp(1, MAX_OBJECT_SCAN_THREADS)
        .min(subtrees)
}

/// Start one scoped scan worker, reporting the OS refusing the thread instead
/// of panicking as `Scope::spawn` does.
fn spawn_scan_worker<'scope, 'env, T: Send + 'scope>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    work: impl FnOnce() -> T + Send + 'scope,
) -> io::Result<std::thread::ScopedJoinHandle<'scope, T>> {
    #[cfg(any(test, feature = "test-support"))]
    if refuse_worker_spawn_for_test() {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    std::thread::Builder::new()
        .name("orbit-git-scan".into())
        .spawn_scoped(scope, work)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static REFUSED_SPAWNS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Per-thread seam that makes every scan worker spawn fail the way an
/// exhausted thread limit does, without placing load on the host. While held,
/// the scan always takes its threaded path so the failure is reached.
#[cfg(any(test, feature = "test-support"))]
pub struct GitWorkerSpawnRefusal;

#[cfg(any(test, feature = "test-support"))]
impl GitWorkerSpawnRefusal {
    pub fn install() -> Self {
        REFUSED_SPAWNS.with(|slot| slot.set(Some(0)));
        Self
    }

    /// Worker spawns refused since [`Self::install`].
    pub fn refused(&self) -> usize {
        REFUSED_SPAWNS.with(|slot| slot.get().unwrap_or(0))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for GitWorkerSpawnRefusal {
    fn drop(&mut self) {
        REFUSED_SPAWNS.with(|slot| slot.set(None));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn worker_spawn_refused_for_test() -> bool {
    REFUSED_SPAWNS.with(|slot| slot.get().is_some())
}

#[cfg(any(test, feature = "test-support"))]
fn refuse_worker_spawn_for_test() -> bool {
    REFUSED_SPAWNS.with(|slot| {
        slot.get().is_some_and(|count| {
            slot.set(Some(count + 1));
            true
        })
    })
}

fn metadata_error(path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "inspect protected Git metadata `{}`: {error}",
        path.display()
    ))
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GitScanStage {
    ReadDirectory,
    InspectEntry,
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
type GitScanHook = Box<dyn FnMut(GitScanStage, &Path) -> io::Result<()>>;

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
thread_local! {
    static SCAN_HOOK: std::cell::RefCell<Option<GitScanHook>> = const { std::cell::RefCell::new(None) };
}

/// Per-thread interleaving seam; production always uses the real filesystem.
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub struct GitScanHookGuard(Option<GitScanHook>);

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
impl GitScanHookGuard {
    pub fn install(hook: impl FnMut(GitScanStage, &Path) -> io::Result<()> + 'static) -> Self {
        Self(SCAN_HOOK.with(|slot| slot.replace(Some(Box::new(hook)))))
    }
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
impl Drop for GitScanHookGuard {
    fn drop(&mut self) {
        SCAN_HOOK.with(|slot| slot.replace(self.0.take()));
    }
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
fn scan_hook_installed() -> bool {
    SCAN_HOOK.with(|slot| slot.borrow().is_some())
}

#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
fn run_scan_hook(stage: GitScanStage, path: &Path) -> io::Result<()> {
    SCAN_HOOK.with(|slot| match slot.borrow_mut().as_mut() {
        Some(hook) => hook(stage, path),
        None => Ok(()),
    })
}
