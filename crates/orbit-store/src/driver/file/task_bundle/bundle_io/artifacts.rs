//! Artifact manifests and blobs: manifest reads with optional payload
//! verification, blob copies and manifest-to-file validation.

use crate::fs::yaml::parse_yaml_with;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, create_private_dir_all};
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACT_MANIFEST_FILE_NAME,
    TASK_ARTIFACTS_DIR_NAME,
};
use sha2::{Digest, Sha256};
use std::fs::{self};
use std::path::Path;

/// Whether a bundle read must hash every `artifacts/files/**` payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArtifactPayloadCheck {
    /// Canonical full-read: open each blob and verify size plus sha256.
    Verify,
    /// Listing/search materialization: parse the manifest, skip payload bytes.
    Defer,
}

#[cfg(test)]
thread_local! {
    static ARTIFACT_PAYLOAD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn record_artifact_payload_read() {
    #[cfg(test)]
    ARTIFACT_PAYLOAD_READS.with(|count| count.set(count.get() + 1));
}

/// Number of artifact payload files opened by strict bundle verification on
/// this thread since the previous take. Listing tests use this to prove the
/// lightweight path does not touch blob bytes.
#[cfg(test)]
pub(crate) fn take_artifact_payload_reads() -> usize {
    ARTIFACT_PAYLOAD_READS.with(|count| count.replace(0))
}

pub(super) fn read_artifact_manifest(
    bundle_dir: &Path,
    payloads: ArtifactPayloadCheck,
) -> Result<Option<ArtifactManifestV2>, OrbitError> {
    let artifact_dir = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
    if !artifact_dir.is_dir() {
        return Err(OrbitError::Store(format!(
            "missing artifact directory {}",
            artifact_dir.display()
        )));
    }

    let manifest_path = artifact_dir.join(TASK_ARTIFACT_MANIFEST_FILE_NAME);
    match fs::read_to_string(&manifest_path) {
        Ok(raw) => {
            let manifest: ArtifactManifestV2 = parse_yaml_with(&raw, &manifest_path, |_, err| {
                OrbitError::Store(format!(
                    "invalid artifact manifest {}: {err}",
                    manifest_path.display()
                ))
            })?;
            manifest.validate()?;
            if payloads == ArtifactPayloadCheck::Verify {
                validate_artifact_manifest_files(&artifact_dir, &manifest)?;
            }
            Ok(Some(manifest))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(OrbitError::Io(err.to_string())),
    }
}

/// Copy every blob referenced by `manifest` from `source_bundle_dir` into
/// `dest_bundle_dir`, validating source content against the manifest as it
/// goes. Used by `orbit task import` to round-trip `artifacts/files/**`
/// alongside the manifest — [`write_bundle_at`] intentionally writes only the
/// manifest so callers can decide where blob bytes come from (a fresh
/// `TaskBundleV2` has none; import staging supplies them from the extracted
/// archive).
///
/// This is also the primitive to reach for when backfilling blobs onto an
/// already-landed bundle whose files were lost (see the module-level docs on
/// backfill in `task_migration::mod`).
pub(crate) fn copy_artifact_blobs(
    source_bundle_dir: &Path,
    dest_bundle_dir: &Path,
    manifest: &ArtifactManifestV2,
) -> Result<(), OrbitError> {
    if manifest.files.is_empty() {
        return Ok(());
    }
    let source_artifact_dir = source_bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
    let dest_artifact_dir = dest_bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
    let dest_files_dir = dest_artifact_dir.join(TASK_ARTIFACT_FILES_DIR_NAME);
    create_private_dir_all(&dest_files_dir)
        .map_err(|err| OrbitError::from_write_io(&dest_files_dir, err))?;
    for file in &manifest.files {
        let source = source_artifact_dir.join(&file.blob);
        let bytes = fs::read(&source).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                OrbitError::Store(format!(
                    "artifact source missing for blob {}",
                    source.display()
                ))
            } else {
                OrbitError::Io(err.to_string())
            }
        })?;
        if bytes.len() as u64 != file.size_bytes {
            return Err(OrbitError::Store(format!(
                "artifact source size mismatch for {}",
                source.display()
            )));
        }
        let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
        if actual_sha256 != file.sha256 {
            return Err(OrbitError::Store(format!(
                "artifact source sha256 mismatch for {}",
                source.display()
            )));
        }
        let dest = dest_artifact_dir.join(&file.blob);
        atomic_write_bytes(&dest, &bytes).map_err(|err| OrbitError::from_write_io(&dest, err))?;
    }
    Ok(())
}

fn validate_artifact_manifest_files(
    artifact_dir: &Path,
    manifest: &ArtifactManifestV2,
) -> Result<(), OrbitError> {
    for file in &manifest.files {
        let blob_path = artifact_dir.join(&file.blob);
        record_artifact_payload_read();
        let bytes = fs::read(&blob_path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                OrbitError::Store(format!(
                    "artifact manifest references missing file {}",
                    blob_path.display()
                ))
            } else {
                OrbitError::Io(err.to_string())
            }
        })?;
        if bytes.len() as u64 != file.size_bytes {
            return Err(OrbitError::Store(format!(
                "artifact manifest size mismatch for {}",
                blob_path.display()
            )));
        }
        let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
        if actual_sha256 != file.sha256 {
            return Err(OrbitError::Store(format!(
                "artifact manifest sha256 mismatch for {}",
                blob_path.display()
            )));
        }
    }
    Ok(())
}
