//! Layout version marker and companion compatibility record I/O.

use std::path::{Path, PathBuf};

use orbit_common::fs::io::atomic_write_text;
use orbit_common::{OrbitError, StorageLayer};
use serde::{Deserialize, Serialize};

use super::registry::LayoutMigration;
use crate::contracts::{CompatibilityRecord, CompatibilityRefusal};

/// Marker file recording the workspace's current layout version, relative to
/// the workspace `.orbit` directory. Lives under `state/` (gitignored
/// runtime state) so stamping a workspace never dirties repositories that
/// commit parts of `.orbit/`.
const MARKER_FILE: &str = "layout.version";

/// Companion forward-compatibility record, relative to `state/`. Written
/// beside the marker whenever a migration advances it, so a binary that is
/// older than the workspace can tell an additive layout bump from a breaking
/// one (ORB-12434). A separate file, not extra fields in the marker: shipped
/// binaries parse the whole marker as one integer, and a marker they cannot
/// parse would be a worse failure than the one this contract replaces.
const COMPAT_FILE: &str = "layout.compat";

/// The executable that last advanced the marker, relative to `state/`, so a
/// binary refused by a newer layout can name what advanced it [ORB-20262].
/// Diagnostic only: nothing decides admission from it.
const WRITER_FILE: &str = "layout.writer";

/// Advisory lock serializing concurrent upgraders, relative to `state/`.
const UPGRADE_LOCK_FILE: &str = "layout.lock";

fn marker_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(MARKER_FILE)
}

pub(super) fn upgrade_lock_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(UPGRADE_LOCK_FILE)
}

fn writer_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(WRITER_FILE)
}

#[derive(Serialize, Deserialize)]
struct LayoutWriter {
    layout_version: u32,
    executable: PathBuf,
}

/// Record this executable as the one that advanced the layout to `version`.
/// Best effort: the record only improves a later refusal's diagnosis.
pub(super) fn record_layout_writer(orbit_dir: &Path, version: u32) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let path = writer_path(orbit_dir);
    let written = serde_json::to_string(&LayoutWriter {
        layout_version: version,
        executable,
    })
    .map_err(std::io::Error::other)
    .and_then(|record| atomic_write_text(&path, &format!("{record}\n")));
    if let Err(error) = written {
        orbit_common::tracing::warn!(
            target: "orbit.store.layout",
            path = %path.display(),
            error = %error,
            "skipped layout writer record"
        );
    }
}

/// The executable recorded as having advanced the layout to `version`.
pub(super) fn layout_writer(orbit_dir: &Path, version: u32) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(writer_path(orbit_dir)).ok()?;
    let writer = serde_json::from_str::<LayoutWriter>(raw.trim()).ok()?;
    (writer.layout_version == version).then_some(writer.executable)
}

fn compat_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join("state").join(COMPAT_FILE)
}

/// Read the companion compatibility record. A missing file is the
/// pre-ORB-12434 case, not an error.
pub(super) fn read_compat_record(
    orbit_dir: &Path,
) -> Result<Result<Option<CompatibilityRecord>, CompatibilityRefusal>, OrbitError> {
    let path = compat_path(orbit_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(None)),
        Err(error) => {
            return Err(OrbitError::storage_io(
                StorageLayer::Migration,
                &error,
                format!(
                    "cannot read layout compatibility record '{}': {error}",
                    path.display()
                ),
            ));
        }
    };
    Ok(CompatibilityRecord::decode(raw.trim()).map(Some))
}

/// Record what this binary knows about layout compatibility, so a binary
/// that is older than `version` can tell whether it may still read the
/// workspace. Written after the marker: a crash in between leaves a stale
/// record, which readers refuse rather than misinterpret.
pub(super) fn write_compat_record(
    orbit_dir: &Path,
    migrations: &[LayoutMigration],
    version: u32,
) -> Result<(), OrbitError> {
    let record = CompatibilityRecord::for_registry(
        version,
        migrations
            .iter()
            .map(|migration| (migration.version, migration.name, migration.compat)),
    );
    let path = compat_path(orbit_dir);
    atomic_write_text(&path, &format!("{}\n", record.encode()?)).map_err(|error| {
        OrbitError::storage_io(
            StorageLayer::Migration,
            &error,
            format!(
                "cannot write layout compatibility record '{}': {error}",
                path.display()
            ),
        )
    })
}

pub(super) fn read_marker(orbit_dir: &Path) -> Result<u32, OrbitError> {
    let path = marker_path(orbit_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(OrbitError::storage_io(
                StorageLayer::Migration,
                &error,
                format!(
                    "cannot read layout version marker '{}': {error}",
                    path.display()
                ),
            ));
        }
    };
    raw.trim().parse::<u32>().map_err(|_| {
        OrbitError::Migration(format!(
            "corrupt layout version marker '{}': {:?} is not a version number \
             (delete the file to re-adopt from version 0 — migrations are idempotent)",
            path.display(),
            raw.trim()
        ))
    })
}

/// Advance the marker with file and parent-directory fsync, so a crash
/// leaves either the old or the new version, never a torn marker.
pub(super) fn write_marker(orbit_dir: &Path, version: u32) -> Result<(), OrbitError> {
    let path = marker_path(orbit_dir);
    atomic_write_text(&path, &format!("{version}\n")).map_err(|error| {
        OrbitError::storage_io(
            StorageLayer::Migration,
            &error,
            format!(
                "cannot write layout version marker '{}': {error}",
                path.display()
            ),
        )
    })
}
