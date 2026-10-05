//! tar.zst packing and extraction for task-migration archives.
//!
//! The archive layout is intentionally simple and standard-tool inspectable:
//! a top-level `manifest.json` plus one `bundles/<ORB-id>/` tree per exported
//! task, copied verbatim from the canonical bundle directory. Bundles carry no
//! index state — the manifest is the only non-bundle entry.

use std::fs::File;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::with_shared_file_lock;

use crate::driver::file::task_bundle::{PENDING_WRITE_FILE_NAME, bundle_lock_target};

/// Compression level for the zstd frame. Task bundles are small text; a moderate
/// level keeps archives compact without a slow compress path.
const ZSTD_LEVEL: i32 = 3;

/// Archive-relative directory that holds the per-task bundle trees.
pub(super) const BUNDLES_DIR: &str = "bundles";
/// Archive-relative path of the manifest entry.
pub(super) const MANIFEST_ENTRY: &str = "manifest.json";

/// Pack `manifest_json` plus each `(task_id, canonical_dir)` bundle tree into a
/// tar.zst archive at `out_path`.
pub(super) fn write_archive(
    out_path: &Path,
    manifest_json: &[u8],
    bundle_dirs: &[(String, PathBuf)],
) -> Result<(), OrbitError> {
    // Fail before opening or truncating the destination when an interrupted
    // writer left recovery evidence behind. Recheck while packing below to
    // cover a writer that starts after this preflight.
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

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    let file = File::create(out_path).map_err(|e| {
        OrbitError::Io(format!(
            "failed to create archive '{}': {e}",
            out_path.display()
        ))
    })?;
    let encoder =
        zstd::stream::write::Encoder::new(file, ZSTD_LEVEL).map_err(map_io("zstd encoder"))?;
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

/// Extract a tar.zst archive into `dest`. Path-traversal entries are rejected by
/// the tar reader, so `dest` fully contains the extracted tree.
///
/// Only directories and regular files are extracted. Exports never contain
/// anything else, and a link would let an archive author have a later read
/// (blob verification, the copy into the store) follow it to a host file.
pub(super) fn extract_archive(archive_path: &Path, dest: &Path) -> Result<(), OrbitError> {
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
    let extract_error = |e: std::io::Error| {
        OrbitError::Store(format!(
            "failed to extract archive '{}': {e}",
            archive_path.display()
        ))
    };
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries().map_err(extract_error)? {
        let mut entry = entry.map_err(extract_error)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        if !(kind.is_file() || kind.is_dir()) {
            return Err(OrbitError::Store(format!(
                "archive '{}' holds an entry that is a link or special file; only files and \
                 directories are imported",
                archive_path.display()
            )));
        }
        entry.unpack_in(dest).map_err(extract_error)?;
    }
    Ok(())
}

fn map_io(context: &'static str) -> impl Fn(std::io::Error) -> OrbitError {
    move |e| OrbitError::Io(format!("{context}: {e}"))
}
