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
            append_bundle_tree(&mut builder, &arcname, dir)
        })?;
    }

    let encoder = builder.into_inner().map_err(map_io("finalize tar"))?;
    encoder.finish().map_err(map_io("finalize zstd"))?;
    Ok(())
}

/// Append one canonical bundle while its shared lock is held. The pending-write
/// record is recovery machinery, not bundle content; all other dotfiles remain
/// eligible payloads and must round-trip unchanged.
fn append_bundle_tree<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    archive_dir: &str,
    source_dir: &Path,
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
        if name == PENDING_WRITE_FILE_NAME {
            continue;
        }

        let source = entry.path();
        let archive_path = format!("{archive_dir}/{}", name.to_string_lossy());
        if entry
            .file_type()
            .map_err(map_io("inspect bundle entry"))?
            .is_dir()
        {
            append_bundle_tree(builder, &archive_path, &source)?;
        } else {
            builder
                .append_path_with_name(&source, archive_path)
                .map_err(map_io("append bundle entry"))?;
        }
    }

    Ok(())
}

/// Extract a tar.zst archive into `dest`. Path-traversal entries are rejected by
/// the tar reader, so `dest` fully contains the extracted tree.
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
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(dest).map_err(|e| {
        OrbitError::Store(format!(
            "failed to extract archive '{}': {e}",
            archive_path.display()
        ))
    })?;
    Ok(())
}

fn map_io(context: &'static str) -> impl Fn(std::io::Error) -> OrbitError {
    move |e| OrbitError::Io(format!("{context}: {e}"))
}
