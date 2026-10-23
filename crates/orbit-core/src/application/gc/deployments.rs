//! Retention for host-level deployment leftovers under the global Orbit root.
//!
//! Deploy procedures leave working directories under `deployments/`, copies
//! of replaced executables in `bin/`, and copies of the host store beside
//! it. None of them is run state, so no other GC class owns them. Each class
//! keeps its newest entries, and nothing that a rollback could still need is
//! a candidate: `<executable>.previous` and any deployment whose candidate
//! manifest names the bytes installed now or at the rollback path.

use std::path::PathBuf;

use serde::Serialize;

use crate::{OrbitError, OrbitRuntime};

/// The retention applied when the caller does not choose one.
pub const DEFAULT_DEPLOYMENT_KEEP: usize = 2;

/// Which kind of leftover an entry is. Retention counts each kind separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentGcClass {
    /// A top-level entry of `<global>/deployments/`.
    Deployment,
    /// A copy of a replaced executable in `<global>/bin/`.
    BinaryBackup,
    /// A copy of the host store, `<global>/orbit.db.bak*`, or one of its sidecars.
    StoreBackup,
}

impl DeploymentGcClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deployment => "deployment",
            Self::BinaryBackup => "binary_backup",
            Self::StoreBackup => "store_backup",
        }
    }

    /// Report order, and the index of the class's retention counter.
    fn rank(self) -> usize {
        match self {
            Self::Deployment => 0,
            Self::BinaryBackup => 1,
            Self::StoreBackup => 2,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DeploymentGcEntry {
    pub class: DeploymentGcClass,
    #[serde(serialize_with = "display_path")]
    pub path: PathBuf,
    /// `kept`, `would_remove` or `removed`.
    pub action: &'static str,
    /// Why the entry is kept or selected: `newest`, `rollback_target`,
    /// `installed_candidate`, `rollback_candidate` or `beyond_retention`.
    pub reason: &'static str,
    /// Bytes of the entry and everything beneath it, without following links.
    pub bytes: u64,
    /// Newest modification time of the entry or anything beneath it.
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeploymentGcResult {
    #[serde(serialize_with = "display_path")]
    pub root: PathBuf,
    pub dry_run: bool,
    /// Newest entries kept per class, besides the rollback-pinned ones.
    pub keep: usize,
    pub entries_removed: usize,
    pub bytes_reclaimable: u64,
    pub bytes_reclaimed: u64,
    pub bytes_kept: u64,
    pub entries: Vec<DeploymentGcEntry>,
}

fn display_path<S: serde::Serializer>(
    path: &std::path::Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

impl OrbitRuntime {
    /// Preview or remove deployment directories, executable backups and
    /// store backups beyond the newest `keep` of each class. The rollback
    /// executable and the deployments that built the installed or rollback
    /// bytes are always kept and never count toward `keep`.
    pub fn gc_deployments(
        &self,
        delete: bool,
        keep: usize,
    ) -> Result<DeploymentGcResult, OrbitError> {
        if keep == 0 {
            return Err(OrbitError::InvalidInput(
                "deployment retention must keep at least one entry of each class".into(),
            ));
        }
        let root = self.global_root();
        let root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => root,
            Err(error) => return Err(error.into()),
        };
        let mut result = DeploymentGcResult {
            root,
            dry_run: !delete,
            keep,
            entries_removed: 0,
            bytes_reclaimable: 0,
            bytes_reclaimed: 0,
            bytes_kept: 0,
            entries: Vec::new(),
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        filesystem::collect(&mut result)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(OrbitError::InvalidInput(
            "deployment collection requires Linux or macOS directory-relative filesystem \
             operations"
                .into(),
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        Ok(result)
    }
}

/// Name markers that identify a copy of a replaced executable. The installed
/// executable, its `.previous` rollback copy and other tools never carry one.
const BACKUP_MARKERS: [&str; 5] = [".bak", ".before-", ".pre-", ".stale", "-stale-"];

/// The executable `orbit update` installs and backs up to `.previous`.
const INSTALLED_NAME: &str = "orbit";
const ROLLBACK_SUFFIX: &str = ".previous";
const STORE_BACKUP_PREFIX: &str = "orbit.db.bak";
const CANDIDATE_MANIFEST_KIND: &str = "orbit-local-candidate";
/// Candidate manifests are a few hundred bytes; anything larger is not one.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

fn is_binary_backup(name: &str) -> bool {
    BACKUP_MARKERS.iter().any(|marker| name.contains(marker))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod filesystem {
    use std::cmp::Reverse;
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString, OsStr};
    use std::fs::File;
    use std::io::{self, Read};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use orbit_common::security::release::normalize_sha256;
    use orbit_types::identity::lower_hex;
    use sha2::{Digest, Sha256};

    use super::super::tmp::filesystem::{Tally, names, open_directory, walk};
    use super::{
        CANDIDATE_MANIFEST_KIND, DeploymentGcClass, DeploymentGcEntry, DeploymentGcResult,
        INSTALLED_NAME, MAX_MANIFEST_BYTES, ROLLBACK_SUFFIX, STORE_BACKUP_PREFIX, is_binary_backup,
    };
    use crate::OrbitError;

    /// Where an entry lives, so removal addresses it through its pinned parent.
    #[derive(Clone, Copy)]
    enum Parent {
        Root,
        Deployments,
        Bin,
    }

    struct Candidate {
        class: DeploymentGcClass,
        parent: Parent,
        /// Retention key: a store backup and its sidecars share this name.
        name: CString,
        /// Measured paths sharing one retention decision, reported separately.
        entries: Vec<(CString, Tally)>,
        /// Set when a rollback could still need this entry.
        pinned: Option<&'static str>,
    }

    impl Candidate {
        fn newest(&self) -> Option<(i64, i64)> {
            self.entries.iter().map(|(_, tally)| tally.newest).max()
        }
    }

    struct Directories {
        root: OwnedFd,
        deployments: Option<OwnedFd>,
        bin: Option<OwnedFd>,
    }

    impl Directories {
        fn get(&self, parent: Parent) -> Option<&OwnedFd> {
            match parent {
                Parent::Root => Some(&self.root),
                Parent::Deployments => self.deployments.as_ref(),
                Parent::Bin => self.bin.as_ref(),
            }
        }
    }

    pub(super) fn collect(result: &mut DeploymentGcResult) -> Result<(), OrbitError> {
        let root_name = CString::new(result.root.as_os_str().as_bytes())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let root = match open_directory(libc::AT_FDCWD, &root_name) {
            Ok(root) => root,
            // No Orbit home means nothing to measure or remove, in either mode.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let directories = Directories {
            deployments: optional_directory(&result.root, &root, c"deployments")?,
            bin: optional_directory(&result.root, &root, c"bin")?,
            root,
        };
        let mut candidates = Vec::new();
        let mut rollback_digests = Vec::new();
        if let Some(bin) = &directories.bin {
            rollback_digests = installed_digests(bin)?;
            binary_backups(bin, &mut candidates)?;
        }
        if let Some(deployments) = &directories.deployments {
            deployment_entries(deployments, &rollback_digests, &mut candidates)?;
        }
        store_backups(&directories.root, &mut candidates)?;
        select(result, &directories, candidates)
    }

    fn optional_directory(
        root: &Path,
        parent: &OwnedFd,
        name: &CStr,
    ) -> Result<Option<OwnedFd>, OrbitError> {
        match open_directory(parent.as_raw_fd(), name) {
            Ok(directory) => Ok(Some(directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            // A symlinked or non-directory class root is refused rather than
            // followed or skipped: collection would otherwise leave the pinned
            // tree for wherever the link points.
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                Err(OrbitError::InvalidInput(format!(
                    "refusing to collect '{}': it is a symbolic link or not a directory, and \
                     deployment collection never follows links",
                    entry_path(root, Parent::Root, name).display()
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The installed executable's digest, then the rollback copy's, each
    /// paired with the reason a deployment that built those bytes is kept.
    fn installed_digests(bin: &OwnedFd) -> Result<Vec<(String, &'static str)>, OrbitError> {
        let mut digests = Vec::new();
        let rollback = format!("{INSTALLED_NAME}{ROLLBACK_SUFFIX}");
        for (name, reason) in [
            (INSTALLED_NAME.to_string(), "installed_candidate"),
            (rollback, "rollback_candidate"),
        ] {
            let name =
                CString::new(name).map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            let Some(mut file) = open_regular(bin, &name, None)? else {
                continue;
            };
            let mut hasher = Sha256::new();
            let mut buffer = vec![0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            digests.push((lower_hex(hasher.finalize()), reason));
        }
        Ok(digests)
    }

    fn binary_backups(bin: &OwnedFd, candidates: &mut Vec<Candidate>) -> Result<(), OrbitError> {
        for name in names(bin)? {
            let text = name.to_string_lossy();
            if text.starts_with('.') {
                continue;
            }
            let rollback = text.ends_with(ROLLBACK_SUFFIX);
            if !rollback && !is_binary_backup(&text) {
                continue;
            }
            // Only copies of executables: a directory or link with a backup
            // marker is not something an install wrote.
            if !matches!(file_type(bin, &name)?, Some(libc::S_IFREG)) {
                continue;
            }
            candidates.push(Candidate {
                class: DeploymentGcClass::BinaryBackup,
                parent: Parent::Bin,
                entries: vec![(name.clone(), walk(bin, &name, false)?)],
                name,
                pinned: rollback.then_some("rollback_target"),
            });
        }
        Ok(())
    }

    fn deployment_entries(
        deployments: &OwnedFd,
        rollback_digests: &[(String, &'static str)],
        candidates: &mut Vec<Candidate>,
    ) -> Result<(), OrbitError> {
        for name in names(deployments)? {
            if name.to_bytes().starts_with(b".") {
                continue;
            }
            let pinned = if matches!(file_type(deployments, &name)?, Some(libc::S_IFDIR)) {
                let directory = open_directory(deployments.as_raw_fd(), &name)?;
                manifest_digests(&directory)?
                    .into_iter()
                    .find_map(|digest| {
                        rollback_digests
                            .iter()
                            .find(|(installed, _)| *installed == digest)
                            .map(|(_, reason)| *reason)
                    })
            } else {
                None
            };
            candidates.push(Candidate {
                class: DeploymentGcClass::Deployment,
                parent: Parent::Deployments,
                entries: vec![(name.clone(), walk(deployments, &name, false)?)],
                name,
                pinned,
            });
        }
        Ok(())
    }

    /// Executable digests named by the local-candidate manifests directly
    /// inside one deployment directory. Other JSON is ignored.
    fn manifest_digests(directory: &OwnedFd) -> Result<Vec<String>, OrbitError> {
        let mut digests = Vec::new();
        for name in names(directory)? {
            if !name.to_bytes().ends_with(b".json") {
                continue;
            }
            let Some(file) = open_regular(directory, &name, Some(MAX_MANIFEST_BYTES))? else {
                continue;
            };
            let mut bytes = Vec::new();
            file.take(MAX_MANIFEST_BYTES).read_to_end(&mut bytes)?;
            let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            if manifest["kind"] != CANDIDATE_MANIFEST_KIND || manifest["schema_version"] != 1 {
                continue;
            }
            if let Some(digest) = manifest["executable_sha256"]
                .as_str()
                .and_then(|digest| normalize_sha256(digest, "executable_sha256").ok())
            {
                digests.push(digest);
            }
        }
        Ok(digests)
    }

    fn store_backups(root: &OwnedFd, candidates: &mut Vec<Candidate>) -> Result<(), OrbitError> {
        let mut backups: BTreeMap<CString, Vec<(CString, Tally)>> = BTreeMap::new();
        for name in names(root)? {
            if !name.to_bytes().starts_with(STORE_BACKUP_PREFIX.as_bytes()) {
                continue;
            }
            // SQLite sidecar suffixes are appended to the copied database's
            // full name. Rank even an orphaned sidecar pair as a single set.
            let bytes = name.to_bytes();
            let backup = bytes
                .strip_suffix(b"-wal")
                .or_else(|| bytes.strip_suffix(b"-shm"))
                .unwrap_or(bytes);
            let backup = CString::new(backup)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            let tally = walk(root, &name, false)?;
            backups.entry(backup).or_default().push((name, tally));
        }
        for (name, mut entries) in backups {
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            candidates.push(Candidate {
                class: DeploymentGcClass::StoreBackup,
                parent: Parent::Root,
                entries,
                name,
                pinned: None,
            });
        }
        Ok(())
    }

    /// Apply the per-class retention, then remove what it selects. Every
    /// entry is measured before anything is removed.
    fn select(
        result: &mut DeploymentGcResult,
        directories: &Directories,
        mut candidates: Vec<Candidate>,
    ) -> Result<(), OrbitError> {
        candidates.sort_by(|left, right| {
            (left.class.rank(), Reverse(left.newest()), &left.name).cmp(&(
                right.class.rank(),
                Reverse(right.newest()),
                &right.name,
            ))
        });
        let mut kept_per_class = [0_usize; 3];
        let mut selected = Vec::new();
        for candidate in &candidates {
            let reason = match candidate.pinned {
                Some(reason) => reason,
                None if kept_per_class[candidate.class.rank()] < result.keep => {
                    kept_per_class[candidate.class.rank()] += 1;
                    "newest"
                }
                None => "beyond_retention",
            };
            for (name, tally) in &candidate.entries {
                let bytes = tally.bytes;
                let action = if reason == "beyond_retention" {
                    result.bytes_reclaimable = result.bytes_reclaimable.saturating_add(bytes);
                    selected.push((candidate.parent, name, result.entries.len()));
                    "would_remove"
                } else {
                    result.bytes_kept = result.bytes_kept.saturating_add(bytes);
                    "kept"
                };
                result.entries.push(DeploymentGcEntry {
                    class: candidate.class,
                    path: entry_path(&result.root, candidate.parent, name),
                    action,
                    reason,
                    bytes,
                    modified_at: modified_at(tally.newest),
                });
            }
        }
        if result.dry_run {
            return Ok(());
        }
        for (parent, name, index) in selected {
            let Some(parent) = directories.get(parent) else {
                continue;
            };
            let entry = &mut result.entries[index];
            entry.bytes = walk(parent, name, true)?.bytes;
            entry.action = "removed";
            result.entries_removed += 1;
            result.bytes_reclaimed = result.bytes_reclaimed.saturating_add(entry.bytes);
        }
        Ok(())
    }

    fn entry_path(root: &Path, parent: Parent, name: &CStr) -> std::path::PathBuf {
        let directory = match parent {
            Parent::Root => root.to_path_buf(),
            Parent::Deployments => root.join("deployments"),
            Parent::Bin => root.join("bin"),
        };
        directory.join(OsStr::from_bytes(name.to_bytes()))
    }

    fn modified_at((seconds, nanoseconds): (i64, i64)) -> Option<String> {
        let nanoseconds = u32::try_from(nanoseconds).ok()?;
        chrono::DateTime::from_timestamp(seconds, nanoseconds)
            .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }

    /// The `S_IFMT` type of one entry, read from the entry itself.
    fn file_type(parent: &OwnedFd, name: &CStr) -> io::Result<Option<libc::mode_t>> {
        let mut metadata = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live, name is NUL-terminated, and fstatat writes
        // the stat only on success. AT_SYMLINK_NOFOLLOW reads a link itself.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: fstatat succeeded and initialized metadata.
        Ok(Some(
            unsafe { metadata.assume_init() }.st_mode & libc::S_IFMT,
        ))
    }

    /// Open one regular file of `parent` without following a link, or `None`
    /// when it is missing, not a regular file, or larger than `limit`.
    fn open_regular(parent: &OwnedFd, name: &CStr, limit: Option<u64>) -> io::Result<Option<File>> {
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        // SAFETY: name is NUL-terminated and parent is a live directory fd.
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT | libc::ELOOP | libc::ENXIO) => Ok(None),
                _ => Err(error),
            };
        }
        // SAFETY: openat returned a new descriptor owned by this caller.
        let file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let metadata = file.metadata()?;
        if !metadata.is_file() || limit.is_some_and(|limit| metadata.len() > limit) {
            return Ok(None);
        }
        Ok(Some(file))
    }
}
