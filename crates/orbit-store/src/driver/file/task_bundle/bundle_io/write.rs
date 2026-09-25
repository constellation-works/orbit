//! Creation and replacement of whole task bundles: staged in a sibling
//! directory, made durable, then published by rename.

use super::super::types::TaskBundleV2;
use crate::driver::file::task_bundle::bundle_io::artifacts::ArtifactPayloadCheck;
use crate::driver::file::task_bundle::bundle_io::copy_artifact_blobs;
use crate::driver::file::task_bundle::bundle_io::jsonl::write_jsonl_file;
use crate::driver::file::task_bundle::bundle_io::read::read_bundle_for_id;
use crate::driver::file::task_bundle::bundle_io::read::validate_bundle;
use crate::driver::file::task_bundle::bundle_io::read::validate_bundle_dir_matches_task_id;
use crate::fs::yaml::write_yaml_durable_with;
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    atomic_write_text, create_private_dir, create_private_dir_all, sync_parent_dir,
};
use orbit_types::task::{
    TASK_ACCEPTANCE_FILE_NAME, TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACT_MANIFEST_FILE_NAME,
    TASK_ARTIFACTS_DIR_NAME, TASK_COMMENTS_FILE_NAME, TASK_DESCRIPTION_FILE_NAME,
    TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME, TASK_EXECUTION_SUMMARY_FILE_NAME,
    TASK_PLAN_FILE_NAME,
};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static STAGING_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write a new v2 bundle at `bundle_dir`.
///
/// The complete bundle is assembled in a unique sibling directory and renamed
/// into place only after every required file and directory is durable. This is
/// a creation-only primitive and refuses to write into an existing bundle
/// directory. Use narrower update helpers for later mutations.
pub(crate) fn write_bundle_at(bundle_dir: &Path, bundle: &TaskBundleV2) -> Result<(), OrbitError> {
    refuse_existing_bundle(bundle_dir)?;
    write_bundle_atomically(bundle_dir, bundle, None, publish_staged_bundle)
}

/// Write a complete imported bundle, including every blob in its artifact
/// manifest, before publishing the destination directory.
pub(crate) fn write_bundle_with_artifacts_at(
    bundle_dir: &Path,
    bundle: &TaskBundleV2,
    source_bundle_dir: &Path,
) -> Result<(), OrbitError> {
    refuse_existing_bundle(bundle_dir)?;
    write_bundle_atomically(
        bundle_dir,
        bundle,
        Some(source_bundle_dir),
        publish_staged_bundle,
    )
}

/// Replace the bundle published at `bundle_dir` with `bundle`.
///
/// Used by owner-wins task import, where the incoming copy comes from the
/// task's owning host and supersedes the local mirror wholesale. The
/// replacement is staged and verified exactly like a fresh write, so the
/// destination only ever holds a complete bundle. A missing canonical path is
/// recreated because its registry binding still identifies the owner-wins
/// mirror. `source_bundle_dir` supplies the artifact blobs the incoming
/// manifest references.
pub(crate) fn replace_bundle_at(
    bundle_dir: &Path,
    bundle: &TaskBundleV2,
    source_bundle_dir: &Path,
) -> Result<(), OrbitError> {
    write_bundle_atomically(
        bundle_dir,
        bundle,
        Some(source_bundle_dir),
        publish_replacement_bundle,
    )
}

/// Creation refuses to write into a bundle directory that already exists;
/// superseding one is [`replace_bundle_at`]'s job.
fn refuse_existing_bundle(bundle_dir: &Path) -> Result<(), OrbitError> {
    if bundle_dir.exists() {
        return Err(OrbitError::Store(format!(
            "task bundle already exists at {}",
            bundle_dir.display()
        )));
    }
    Ok(())
}

pub(super) fn write_bundle_atomically<F>(
    bundle_dir: &Path,
    bundle: &TaskBundleV2,
    artifact_source: Option<&Path>,
    publish: F,
) -> Result<(), OrbitError>
where
    F: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    validate_bundle_dir_matches_task_id(bundle_dir, &bundle.envelope.id)?;
    validate_bundle(bundle)?;
    let staging_dir = create_staging_dir(bundle_dir)?;
    let result = (|| {
        write_bundle_contents(&staging_dir, bundle)?;
        if let Some(manifest) = &bundle.artifact_manifest
            && !manifest.files.is_empty()
        {
            let source = artifact_source.ok_or_else(|| {
                OrbitError::Store(format!(
                    "task bundle {} has artifact files but no artifact source was supplied",
                    bundle.envelope.id
                ))
            })?;
            copy_artifact_blobs(source, &staging_dir, manifest)?;
        }
        read_bundle_for_id(
            &staging_dir,
            &bundle.envelope.id,
            ArtifactPayloadCheck::Verify,
        )?;
        sync_staged_bundle_dirs(&staging_dir)?;
        publish(&staging_dir, bundle_dir).map_err(|err| OrbitError::from_write_io(bundle_dir, err))
    })();

    if let Err(error) = &result {
        cleanup_partial_bundle_best_effort(&staging_dir, "atomic bundle staging", error);
    }
    result
}

fn write_bundle_contents(bundle_dir: &Path, bundle: &TaskBundleV2) -> Result<(), OrbitError> {
    ensure_bundle_dirs(bundle_dir)?;
    write_yaml_durable_with(
        &bundle_dir.join(TASK_ENVELOPE_FILE_NAME),
        &bundle.envelope,
        |err| OrbitError::Store(err.to_string()),
    )?;
    let description_path = bundle_dir.join(TASK_DESCRIPTION_FILE_NAME);
    atomic_write_text(&description_path, &bundle.description)
        .map_err(|err| OrbitError::from_write_io(&description_path, err))?;
    let acceptance_path = bundle_dir.join(TASK_ACCEPTANCE_FILE_NAME);
    atomic_write_text(&acceptance_path, &bundle.acceptance)
        .map_err(|err| OrbitError::from_write_io(&acceptance_path, err))?;
    let plan_path = bundle_dir.join(TASK_PLAN_FILE_NAME);
    atomic_write_text(&plan_path, &bundle.plan)
        .map_err(|err| OrbitError::from_write_io(&plan_path, err))?;
    let execution_summary_path = bundle_dir.join(TASK_EXECUTION_SUMMARY_FILE_NAME);
    atomic_write_text(&execution_summary_path, &bundle.execution_summary)
        .map_err(|err| OrbitError::from_write_io(&execution_summary_path, err))?;

    write_jsonl_file(&bundle_dir.join(TASK_EVENTS_FILE_NAME), &bundle.events)?;
    write_jsonl_file(&bundle_dir.join(TASK_COMMENTS_FILE_NAME), &bundle.comments)?;
    if let Some(manifest) = &bundle.artifact_manifest {
        write_yaml_durable_with(
            &bundle_dir
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_MANIFEST_FILE_NAME),
            manifest,
            |err| OrbitError::Store(err.to_string()),
        )?;
    }

    Ok(())
}

fn create_staging_dir(bundle_dir: &Path) -> Result<PathBuf, OrbitError> {
    let parent = bundle_dir.parent().ok_or_else(|| {
        OrbitError::Store(format!(
            "task bundle path has no parent: {}",
            bundle_dir.display()
        ))
    })?;
    create_private_dir_all(parent).map_err(|error| OrbitError::from_write_io(parent, error))?;

    for _ in 0..32 {
        let candidate = scratch_sibling_path(bundle_dir, "staging")
            .map_err(|error| OrbitError::from_write_io(bundle_dir, error))?;
        match create_private_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(OrbitError::from_write_io(&candidate, error)),
        }
    }
    Err(OrbitError::Store(format!(
        "could not allocate a staging directory for task bundle {}",
        bundle_dir.display()
    )))
}

/// Build a unique hidden sibling path of `bundle_dir` for scratch use during
/// publication (`.<bundle>.<pid>.<sequence>.<suffix>`). The process id and the
/// process-wide counter keep two concurrent writers of the same bundle apart.
fn scratch_sibling_path(bundle_dir: &Path, suffix: &str) -> std::io::Result<PathBuf> {
    let invalid = |detail: &str| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("task bundle path {} {detail}", bundle_dir.display()),
        )
    };
    let parent = bundle_dir
        .parent()
        .ok_or_else(|| invalid("has no parent directory"))?;
    let bundle_name = bundle_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("has no UTF-8 file name"))?;

    let sequence = STAGING_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{bundle_name}.{}.{sequence}.{suffix}",
        std::process::id()
    )))
}

fn sync_staged_bundle_dirs(staging_dir: &Path) -> Result<(), OrbitError> {
    let artifact_dir = staging_dir.join(TASK_ARTIFACTS_DIR_NAME);
    let artifact_files_dir = artifact_dir.join(TASK_ARTIFACT_FILES_DIR_NAME);
    sync_path_parent(&artifact_files_dir)?;
    sync_path_parent(&artifact_dir)?;
    sync_path_parent(staging_dir)
}

fn sync_path_parent(path: &Path) -> Result<(), OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::Store(format!("path has no parent directory: {}", path.display()))
    })?;
    let directory = File::open(parent).map_err(|err| OrbitError::from_write_io(path, err))?;
    sync_parent_dir(&directory).map_err(|err| OrbitError::from_write_io(path, err))
}

fn publish_staged_bundle(staging_dir: &Path, bundle_dir: &Path) -> std::io::Result<()> {
    fs::rename(staging_dir, bundle_dir)?;
    sync_bundle_parent(bundle_dir)
}

/// Publish a staged bundle over an existing one, or recreate a missing one.
/// `rename` cannot overwrite a non-empty directory, so the superseded bundle
/// is moved aside first and dropped only once the replacement is in place; a
/// failed swap puts the original back, so the destination is never left empty.
fn publish_replacement_bundle(staging_dir: &Path, bundle_dir: &Path) -> std::io::Result<()> {
    let retired = if bundle_dir.exists() {
        let retired = scratch_sibling_path(bundle_dir, "retired")?;
        fs::rename(bundle_dir, &retired)?;
        Some(retired)
    } else {
        None
    };

    if let Err(error) = fs::rename(staging_dir, bundle_dir) {
        if let Some(retired) = &retired {
            let _ = fs::rename(retired, bundle_dir);
        }
        return Err(error);
    }
    sync_bundle_parent(bundle_dir)?;

    for retired in retired_bundle_paths(bundle_dir)? {
        fs::remove_dir_all(retired)?;
    }
    Ok(())
}

/// Find interrupted replacement directories for `bundle_dir`.
///
/// These siblings are deliberately ignored while resolving a bundle. Once a
/// replacement has been published and synced, they are safe to remove: the
/// canonical path now contains the complete owner copy.
fn retired_bundle_paths(bundle_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let parent = bundle_dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("no parent dir for {}", bundle_dir.display()),
        )
    })?;
    let bundle_name = bundle_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("no UTF-8 file name for {}", bundle_dir.display()),
            )
        })?;
    let prefix = format!(".{bundle_name}.");

    let mut retired = Vec::new();
    for entry in parent.read_dir()? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".retired") && entry.file_type()?.is_dir() {
            retired.push(entry.path());
        }
    }
    Ok(retired)
}

fn sync_bundle_parent(bundle_dir: &Path) -> std::io::Result<()> {
    let parent = bundle_dir.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("no parent dir for {}", bundle_dir.display()),
        )
    })?;
    let directory = File::open(parent)?;
    sync_parent_dir(&directory)
}

fn ensure_bundle_dirs(bundle_dir: &Path) -> Result<(), OrbitError> {
    create_private_dir_all(bundle_dir).map_err(|err| OrbitError::from_write_io(bundle_dir, err))?;
    let artifact_files_dir = bundle_dir
        .join(TASK_ARTIFACTS_DIR_NAME)
        .join(TASK_ARTIFACT_FILES_DIR_NAME);
    create_private_dir_all(&artifact_files_dir)
        .map_err(|err| OrbitError::from_write_io(&artifact_files_dir, err))?;
    Ok(())
}

pub(super) fn cleanup_partial_bundle(bundle_dir: &Path) -> Result<(), OrbitError> {
    match fs::remove_dir_all(bundle_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(OrbitError::Io(err.to_string())),
    }
}

pub(crate) fn cleanup_partial_bundle_best_effort(
    bundle_dir: &Path,
    phase: &str,
    original: &OrbitError,
) {
    if let Err(cleanup_err) = cleanup_partial_bundle(bundle_dir) {
        orbit_common::tracing::warn!(
            target: "orbit.store.task_bundle_v2",
            bundle_dir = %bundle_dir.display(),
            phase,
            original_error = %original,
            cleanup_error = %cleanup_err,
            "failed to clean up partial task bundle",
        );
    }
}
