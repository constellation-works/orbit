//! On-disk task bundle I/O.
//!
//! `write` stages and publishes whole bundles, `read` loads and validates
//! them, `jsonl` owns the append-only sidecars, `artifacts` the manifest and
//! blobs, `stub` unpublished-stub cleanup, and `commit` the multi-file update
//! protocol.

mod artifacts;
mod commit;
mod jsonl;
mod read;
mod stub;
mod write;

// `commit`, `jsonl` and `read` reach these through `super::`.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::types::TaskBundleV2;
use jsonl::scan_jsonl_records;
use read::{read_required_bytes, read_required_text};

pub(crate) use artifacts::{
    ManifestBlobReadFailure, copy_artifact_blobs, read_verified_manifest_blob,
};
pub(crate) use commit::{
    PENDING_WRITE_FILE_NAME, PendingWriteGuard, publish_envelope, recover_pending_bundle_at,
    recover_pending_write, truncate_jsonl_file,
};
pub(crate) use jsonl::append_jsonl_rows;
pub(crate) use read::{
    read_artifact_manifest_at, read_bundle_at, read_bundle_lightweight_at, read_comments_at,
    read_envelope_at, read_events_at, read_search_docs_at,
};
pub use stub::is_unpublished_stub;
pub(crate) use stub::reap_unpublished_stub;
pub(crate) use write::{
    cleanup_partial_bundle_best_effort, replace_bundle_at, write_bundle_at,
    write_bundle_with_artifacts_at,
};

/// Resolve a task-bundle file beneath a non-symlink bundle directory and reject
/// symlinks or non-regular entries at the target path.
///
/// Ancestor directories above `bundle_dir` (such as a symlinked workspace root
/// or macOS `/var`) are canonicalized, while `bundle_dir` itself and the leaf
/// file must not be symlinks. Missing leaf files are admitted so callers can
/// create or report absence under the canonical bundle directory.
pub(super) fn validated_bundle_file_path(path: &Path) -> io::Result<PathBuf> {
    let Some(file_name) = path.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path '{}' has no file name",
                path.display()
            ),
        ));
    };
    if file_name == "." || file_name == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path '{}' must name a file",
                path.display()
            ),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path '{}' has no parent directory",
                path.display()
            ),
        )
    })?;
    let parent_for_resolution = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = match (
        parent_for_resolution.parent(),
        parent_for_resolution.file_name(),
    ) {
        (Some(grandparent), Some(dir_name)) => {
            let grandparent_for_resolution = if grandparent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                grandparent
            };
            let canonical_grandparent = fs::canonicalize(grandparent_for_resolution)?;
            let checked_parent = canonical_grandparent.join(dir_name);
            if !checked_parent.starts_with(&canonical_grandparent) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "task bundle directory '{}' escapes its parent",
                        path.display()
                    ),
                ));
            }
            let parent_meta = fs::symlink_metadata(&checked_parent)?;
            if parent_meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "task bundle directory must not be a symlink: {}",
                        parent_for_resolution.display()
                    ),
                ));
            }
            fs::canonicalize(&checked_parent)?
        }
        _ => fs::canonicalize(parent_for_resolution)?,
    };
    let canonical_path = canonical_parent.join(file_name);
    if !canonical_path.starts_with(&canonical_parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path '{}' escapes its bundle directory",
                path.display()
            ),
        ));
    }
    match fs::symlink_metadata(&canonical_path) {
        Ok(metadata) => check_bundle_regular_file(path, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(canonical_path)
}

/// Open a task-bundle file without following a final-component symlink and
/// verify the resulting descriptor is a regular file.
pub(super) fn open_bundle_file_no_follow(
    path: &Path,
    options: &mut OpenOptions,
) -> io::Result<File> {
    let canonical_path = validated_bundle_file_path(path)?;
    apply_no_follow_flags(options);
    let file = options.open(&canonical_path)?;
    let metadata = file.metadata()?;
    check_bundle_regular_file(&canonical_path, &metadata)?;
    Ok(file)
}

pub(super) fn read_bundle_file_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    let mut file = open_bundle_file_no_follow(path, &mut options)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn read_bundle_file_string(path: &Path) -> io::Result<String> {
    let bytes = read_bundle_file_bytes(path)?;
    String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn check_bundle_regular_file(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path must not be a symlink: {}",
                path.display()
            ),
        ));
    }
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "task bundle file path must be a regular file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn apply_no_follow_flags(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = options;
    }
}

#[cfg(test)]
mod tests;
