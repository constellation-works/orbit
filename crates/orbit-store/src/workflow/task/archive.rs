//! tar.zst packing and extraction for task-migration archives.
//!
//! The archive layout is intentionally simple and standard-tool inspectable:
//! a top-level `manifest.json` plus one `bundles/<ORB-id>/` tree per exported
//! task, copied verbatim from the canonical bundle directory. Bundles carry no
//! index state — the manifest is the only non-bundle entry.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_shared_file_lock;

use crate::driver::file::task_bundle::{PENDING_WRITE_FILE_NAME, bundle_lock_target};

/// Compression level for the zstd frame. Task bundles are small text; a moderate
/// level keeps archives compact without a slow compress path.
const ZSTD_LEVEL: i32 = 3;

/// Largest tree a task-migration archive may unpack to. Task bundles are
/// small text trees, so this is generous for legitimate exports while making
/// a decompression bomb harmless: a few compressed kilobytes say nothing
/// about how many bytes they inflate to.
pub(super) const MAX_TASK_ARCHIVE_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Most members one task-migration archive may carry, so an archive of
/// millions of empty files cannot exhaust inodes before the size bound trips.
pub(super) const MAX_TASK_ARCHIVE_ENTRIES: usize = 20_000;

/// The bounds one task-archive extraction runs under, carried as a value
/// so both bounds are stated once at the extraction call site.
#[derive(Debug, Clone, Copy)]
pub(super) struct TaskArchiveLimits {
    /// Total bytes an archive may unpack to.
    pub(super) unpacked_bytes: u64,
    /// Number of members an archive may carry.
    pub(super) entries: usize,
}

impl TaskArchiveLimits {
    /// What every import extracts under.
    pub(super) const DEFAULT: Self = Self {
        unpacked_bytes: MAX_TASK_ARCHIVE_UNPACKED_BYTES,
        entries: MAX_TASK_ARCHIVE_ENTRIES,
    };
}

/// Archive-relative directory that holds the per-task bundle trees.
pub(super) const BUNDLES_DIR: &str = "bundles";
/// Archive-relative path of the manifest entry.
pub(super) const MANIFEST_ENTRY: &str = "manifest.json";

/// Pack `manifest_json` plus each `(task_id, canonical_dir)` bundle tree into a
/// tar.zst archive at `out_path`. Replace the destination only after the staged
/// archive is complete and synced; errors drop the staging file.
#[allow(
    clippy::disallowed_methods,
    reason = "task export creates user-selected backup directories, outside Orbit state"
)]
pub(super) fn write_archive(
    out_path: &Path,
    manifest_json: &[u8],
    bundle_dirs: &[(String, PathBuf)],
) -> Result<(), OrbitError> {
    // Fail before staging when an interrupted writer left recovery evidence
    // behind. Recheck while packing to cover a writer starting after preflight.
    for (task_id, dir) in bundle_dirs {
        with_shared_file_lock(&bundle_lock_target(dir), "task migration export", || {
            if !dir.is_dir() {
                return Err(OrbitError::Store(format!(
                    "canonical bundle for '{task_id}' disappeared while exporting at {}",
                    dir.display()
                )));
            }
            reject_pending_write(task_id, dir)?;
            reject_linked_entries(task_id, dir)
        })?;
    }

    let parent = out_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(map_io("create archive directory"))?;
    // A sibling keeps the final replacement on the same filesystem. Keep the
    // guard outside the encoder so every packing/finalization error cleans up.
    let mut staged = tempfile::Builder::new()
        .prefix(".orbit-task-export-")
        .tempfile_in(parent)
        .map_err(|e| {
            OrbitError::Io(format!(
                "failed to stage archive '{}': {e}",
                out_path.display()
            ))
        })?;
    // Retain access permissions when replacing a regular backup. New archives
    // keep the temporary file's private defaults; never follow an output link.
    match std::fs::symlink_metadata(out_path) {
        Ok(metadata) if metadata.is_file() => staged
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(map_io("preserve archive permissions"))?,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(map_io("inspect archive destination")(error)),
    }
    let encoder = zstd::stream::write::Encoder::new(staged.as_file_mut(), ZSTD_LEVEL)
        .map_err(map_io("zstd encoder"))?;
    let mut builder = tar::Builder::new(encoder);
    // A link that appears after the check above is archived as a link, which
    // import refuses, and never as a copy of its target.
    builder.follow_symlinks(false);
    // Deterministic mode zeroes mtimes/uid/gid so archives don't leak host
    // ownership and re-exports of unchanged bundles are stable.
    builder.mode(tar::HeaderMode::Deterministic);

    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_json.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, MANIFEST_ENTRY, manifest_json)
        .map_err(map_io("write manifest entry"))?;

    #[cfg(test)]
    super::tests::archive::after_manifest();

    for (task_id, dir) in bundle_dirs {
        let arcname = format!("{BUNDLES_DIR}/{task_id}");
        with_shared_file_lock(&bundle_lock_target(dir), "task migration export", || {
            if !dir.is_dir() {
                return Err(OrbitError::Store(format!(
                    "canonical bundle for '{task_id}' disappeared while exporting at {}",
                    dir.display()
                )));
            }
            reject_pending_write(task_id, dir)?;
            reject_linked_entries(task_id, dir)?;
            append_bundle_tree(&mut builder, &arcname, dir)
        })?;
    }

    let encoder = builder.into_inner().map_err(map_io("finalize tar"))?;
    encoder.finish().map_err(map_io("finalize zstd"))?;
    orbit_common::fs::io::sync_file(staged.as_file()).map_err(map_io("sync archive"))?;
    staged.persist(out_path).map_err(|e| {
        OrbitError::Io(format!(
            "failed to replace archive '{}': {}",
            out_path.display(),
            e.error
        ))
    })?;
    Ok(())
}

/// Append one canonical bundle while its shared lock is held. Pending-write
/// recovery records are rejected before packing and the bundle-root sidecar is
/// excluded here as defense in depth; nested same-named artifacts and other
/// dotfiles remain eligible payloads and round-trip unchanged.
pub(super) fn append_bundle_tree<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    archive_dir: &str,
    source_dir: &Path,
) -> Result<(), OrbitError> {
    append_bundle_tree_level(builder, archive_dir, source_dir, true)
}

fn append_bundle_tree_level<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    archive_dir: &str,
    source_dir: &Path,
    is_bundle_root: bool,
) -> Result<(), OrbitError> {
    builder
        .append_dir(archive_dir, source_dir)
        .map_err(map_io("append bundle directory"))?;

    let mut entries = std::fs::read_dir(source_dir)
        .map_err(map_io("read bundle directory"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_io("read bundle directory"))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name();
        if is_bundle_root && name == PENDING_WRITE_FILE_NAME {
            continue;
        }

        let source = entry.path();
        let archive_path = format!("{archive_dir}/{}", name.to_string_lossy());
        if entry
            .file_type()
            .map_err(map_io("inspect bundle entry"))?
            .is_dir()
        {
            append_bundle_tree_level(builder, &archive_path, &source, false)?;
        } else {
            builder
                .append_path_with_name(&source, archive_path)
                .map_err(map_io("append bundle entry"))?;
        }
    }

    Ok(())
}

/// Refuse a bundle that contains anything but directories and regular files.
///
/// A bundle is written only by Orbit, so a link inside one was planted. The
/// archive leaves the machine; following the link would pack whatever it
/// points at, such as a credential file, into it. The bundle root is held to
/// the same rule: tar resolves the root it is handed, so a linked root would
/// pack its target's files under the bundle's name.
fn reject_linked_entries(task_id: &str, dir: &Path) -> Result<(), OrbitError> {
    let root = std::fs::symlink_metadata(dir).map_err(map_io("inspect bundle directory"))?;
    if !root.file_type().is_dir() {
        return Err(OrbitError::Store(format!(
            "cannot export task '{task_id}': {} is not a directory",
            dir.display()
        )));
    }
    reject_linked_children(task_id, dir)
}

fn reject_linked_children(task_id: &str, dir: &Path) -> Result<(), OrbitError> {
    for entry in std::fs::read_dir(dir).map_err(map_io("read bundle directory"))? {
        let entry = entry.map_err(map_io("read bundle directory"))?;
        let file_type = entry.file_type().map_err(map_io("inspect bundle entry"))?;
        if file_type.is_dir() {
            reject_linked_children(task_id, &entry.path())?;
        } else if !file_type.is_file() {
            return Err(OrbitError::Store(format!(
                "cannot export task '{task_id}': {} is not a regular file",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn reject_pending_write(task_id: &str, bundle_dir: &Path) -> Result<(), OrbitError> {
    let pending_path = bundle_dir.join(PENDING_WRITE_FILE_NAME);
    if pending_path
        .try_exists()
        .map_err(map_io("inspect pending-write record"))?
    {
        return Err(OrbitError::Store(format!(
            "cannot export task '{task_id}' while pending-write recovery is required at {}; recover or reindex the task first",
            pending_path.display()
        )));
    }
    Ok(())
}

/// Extract a tar.zst archive into `dest`, bounded by
/// [`MAX_TASK_ARCHIVE_UNPACKED_BYTES`] and [`MAX_TASK_ARCHIVE_ENTRIES`].
///
/// A member whose path would escape `dest` is refused explicitly rather
/// than delegated to `tar::Entry::unpack_in`, which silently skips such a
/// member and would leave a silently incomplete staged tree.
///
/// Only directories and regular files are extracted. Exports never contain
/// anything else, and a link would let an archive author have a later read
/// (blob verification, the copy into the store) follow it to a host file.
pub(super) fn extract_archive(archive_path: &Path, dest: &Path) -> Result<(), OrbitError> {
    extract_archive_with_limits(archive_path, dest, TaskArchiveLimits::DEFAULT)
}

pub(super) fn extract_archive_with_limits(
    archive_path: &Path,
    dest: &Path,
    limits: TaskArchiveLimits,
) -> Result<(), OrbitError> {
    let file = File::open(archive_path).map_err(|e| {
        OrbitError::Io(format!(
            "failed to open archive '{}': {e}",
            archive_path.display()
        ))
    })?;
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|e| {
        OrbitError::Store(format!(
            "'{}' is not a valid zstd archive: {e}",
            archive_path.display()
        ))
    })?;
    let extract_error = |e: std::io::Error| extract_failure(archive_path, &e);
    // Bound the decompressed stream itself, so tar headers and any bytes
    // not accounted for by a member's logical size also count against the
    // budget. The per-entry logical-size check below is the sparse-safe
    // half: GNU sparse holes are created by seek/set_len without those
    // bytes ever appearing in the stream.
    let bounded = BoundedReader::new(decoder, limits.unpacked_bytes, archive_path);
    let mut archive = tar::Archive::new(bounded);
    let mut count = 0usize;
    let mut remaining_output = limits.unpacked_bytes;
    for entry in archive.entries().map_err(extract_error)? {
        let mut entry = entry.map_err(extract_error)?;
        count += 1;
        if count > limits.entries {
            return Err(OrbitError::Store(format!(
                "archive '{}' has more than {} entries \
                 (MAX_TASK_ARCHIVE_ENTRIES = {}); refusing to extract it",
                archive_path.display(),
                limits.entries,
                limits.entries
            )));
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        // Logical size: for a GNU sparse member `header.size()` is the
        // real (expanded) size while `entry.size()` is only the stored
        // bytes, so take the larger. Check before `unpack_in` can create
        // an oversized or sparse file, and before the kind refusal so an
        // over-bound sparse member names the unpacked bound.
        let logical_size = entry
            .header()
            .size()
            .map(|header_size| header_size.max(entry.size()))
            .unwrap_or_else(|_| entry.size());
        remaining_output = remaining_output.checked_sub(logical_size).ok_or_else(|| {
            OrbitError::Store(format!(
                "archive '{}' unpacks to more than {} bytes \
                 (MAX_TASK_ARCHIVE_UNPACKED_BYTES = {}); refusing to extract it",
                archive_path.display(),
                limits.unpacked_bytes,
                limits.unpacked_bytes
            ))
        })?;
        if !(kind.is_file() || kind.is_gnu_sparse() || kind.is_dir()) {
            return Err(OrbitError::Store(format!(
                "archive '{}' holds an entry that is a link or special file; only files and \
                 directories are imported",
                archive_path.display()
            )));
        }
        let path = entry.path().map_err(extract_error)?.into_owned();
        refuse_escaping_member(&path, archive_path)?;
        entry.unpack_in(dest).map_err(extract_error)?;
    }
    Ok(())
}

/// Report an extraction failure with its whole cause chain, so a bound
/// enforced by [`BoundedReader`] is not buried under tar's wrapper error.
fn extract_failure(archive_path: &Path, error: &std::io::Error) -> OrbitError {
    let mut causes = vec![error.to_string()];
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(inner) = cause {
        causes.push(inner.to_string());
        cause = inner.source();
    }
    OrbitError::Store(format!(
        "failed to extract archive '{}': {}",
        archive_path.display(),
        causes.join(": ")
    ))
}

/// Refuse a member whose path would write outside the extraction root.
fn refuse_escaping_member(path: &Path, archive_path: &Path) -> Result<(), OrbitError> {
    let refuse = |why: &str| {
        Err(OrbitError::Store(format!(
            "archive '{}' contains the entry '{}', which {why}; refusing to extract it",
            archive_path.display(),
            path.display()
        )))
    };
    if path.components().next().is_none() {
        return refuse("has an empty path");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return refuse("traverses out of the archive root with `..`");
            }
            Component::RootDir | Component::Prefix(_) => {
                return refuse("is an absolute path");
            }
        }
    }
    Ok(())
}

/// A reader that fails once the decompressed stream passes `limit` bytes.
///
/// One byte of headroom is kept so reaching exactly `limit` is still a
/// successful read: the error fires only on the byte that would exceed it.
struct BoundedReader<R> {
    inner: R,
    remaining: u64,
    limit: u64,
    archive_path: PathBuf,
}

impl<R: Read> BoundedReader<R> {
    fn new(inner: R, limit: u64, archive_path: &Path) -> Self {
        Self {
            inner,
            remaining: limit.saturating_add(1),
            limit,
            archive_path: archive_path.to_path_buf(),
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let capacity = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        if capacity == 0 {
            return Err(std::io::Error::other(format!(
                "archive '{}' unpacks to more than {} bytes \
                 (MAX_TASK_ARCHIVE_UNPACKED_BYTES = {})",
                self.archive_path.display(),
                self.limit,
                self.limit
            )));
        }
        let read = self.inner.read(&mut buf[..capacity])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

fn map_io(context: &'static str) -> impl Fn(std::io::Error) -> OrbitError {
    move |e| OrbitError::Io(format!("{context}: {e}"))
}
