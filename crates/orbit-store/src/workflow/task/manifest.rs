//! Migration archive manifest: format version, shape, and the read and
//! validation applied before import mutates anything.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::archive;

/// Archive container-format version. Bumped only when the archive *layout*
/// (manifest shape / entry paths) changes incompatibly.
pub const MIGRATION_FORMAT_VERSION: u32 = 1;

/// Manifest written at the root of every migration archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMigrationManifest {
    /// [`MIGRATION_FORMAT_VERSION`] the archive was written with.
    pub format_version: u32,
    /// [`TASK_ARTIFACT_SCHEMA_VERSION`] of the bundles inside.
    pub task_schema_version: u32,
    /// Workspace id the bundles were exported from.
    pub source_workspace_id: String,
    /// Human-readable slug of the source workspace.
    pub source_workspace_slug: String,
    /// Task ids contained in the archive (canonical `ORB-00000` form).
    pub task_ids: Vec<String>,
    /// When the archive was produced.
    pub exported_at: DateTime<Utc>,
}

pub(super) fn read_manifest(staging: &Path) -> Result<TaskMigrationManifest, OrbitError> {
    let path = staging.join(archive::MANIFEST_ENTRY);
    let raw = std::fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            OrbitError::Store("archive is missing manifest.json".to_string())
        } else {
            OrbitError::Io(e.to_string())
        }
    })?;
    serde_json::from_slice(&raw)
        .map_err(|e| OrbitError::Store(format!("invalid migration manifest: {e}")))
}

pub(super) fn validate_manifest(manifest: &TaskMigrationManifest) -> Result<(), OrbitError> {
    if manifest.format_version != MIGRATION_FORMAT_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "archive format version {} is not supported (this build expects {})",
            manifest.format_version, MIGRATION_FORMAT_VERSION
        )));
    }
    if manifest.task_schema_version != TASK_ARTIFACT_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "archive task schema version {} is not supported (this build expects {})",
            manifest.task_schema_version, TASK_ARTIFACT_SCHEMA_VERSION
        )));
    }
    Ok(())
}
