use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use orbit_common::fs::io::{
    atomic_write_bytes, atomic_write_text, sync_parent_dir, with_exclusive_file_lock,
};
use orbit_common::migration::Plan;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestV2, TASK_ACCEPTANCE_FILE_NAME, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACTS_DIR_NAME, TASK_COMMENTS_FILE_NAME,
    TASK_DESCRIPTION_FILE_NAME, TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME,
    TASK_EXECUTION_SUMMARY_FILE_NAME, TASK_PLAN_FILE_NAME, TaskCommentRowV2, TaskEnvelopeV2,
    TaskEventRowV2,
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::migrations as task_migrations;
use super::types::TaskBundleV2;
use crate::fs::yaml::{parse_yaml_with, write_yaml_durable_with};

mod commit;

pub(crate) use commit::{
    BundleWriteFault, PENDING_WRITE_FILE_NAME, PendingWriteGuard, fail_if_injected,
    publish_envelope, recover_pending_bundle_at,
};

#[cfg(test)]
pub(crate) use commit::inject_bundle_write_faults;

static STAGING_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether a bundle read must hash every `artifacts/files/**` payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactPayloadCheck {
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

fn write_bundle_atomically<F>(
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
    fs::create_dir_all(parent).map_err(|error| OrbitError::from_write_io(parent, error))?;

    for _ in 0..32 {
        let candidate = scratch_sibling_path(bundle_dir, "staging")
            .map_err(|error| OrbitError::from_write_io(bundle_dir, error))?;
        match fs::create_dir(&candidate) {
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

/// Canonical full-bundle read: parse every sidecar and hash every artifact blob.
pub(crate) fn read_bundle_at(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    read_bundle_at_with(bundle_dir, ArtifactPayloadCheck::Verify)
}

/// Assemble a task bundle without opening artifact payload bytes.
///
/// This is the listing/search materialization primitive. It still reads the
/// envelope, markdown bodies, events, comments, and `artifacts/manifest.yaml`,
/// applies the pending-write view, and checks event-log/envelope status
/// consistency. Callers that need a consistent snapshot must take the same
/// canonical bundle lock used by [`read_bundle_at`].
///
/// Deferred checks, performed by [`read_bundle_at`] and by import, reindex,
/// publication restore, and artifact retrieval: existence, size, and sha256 of
/// every `artifacts/files/**` blob named by the manifest.
pub(crate) fn read_bundle_lightweight_at(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    read_bundle_at_with(bundle_dir, ArtifactPayloadCheck::Defer)
}

fn read_bundle_at_with(
    bundle_dir: &Path,
    payloads: ArtifactPayloadCheck,
) -> Result<TaskBundleV2, OrbitError> {
    let expected_task_id = task_id_from_bundle_dir(bundle_dir)?;
    read_bundle_for_id(bundle_dir, &expected_task_id, payloads).map_err(|error| match error {
        OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        }
        | OrbitError::TaskBundleCorrupt { .. }
        | OrbitError::Io(_) => error,
        other => OrbitError::TaskBundleCorrupt {
            task_id: expected_task_id,
            path: bundle_dir.to_string_lossy().into_owned(),
            reason: other.to_string(),
        },
    })
}

/// Read only a bundle's envelope.
///
/// The envelope carries every field the generated task index projects, so
/// index validation reads this instead of assembling the whole bundle — one
/// small YAML file per task rather than the bundle's seven.
pub(crate) fn read_envelope_at(bundle_dir: &Path) -> Result<TaskEnvelopeV2, OrbitError> {
    let expected_task_id = task_id_from_bundle_dir(bundle_dir)?;
    read_envelope_for_id(bundle_dir, &expected_task_id).map_err(|error| match error {
        OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        }
        | OrbitError::TaskBundleCorrupt { .. }
        | OrbitError::Io(_) => error,
        other => OrbitError::TaskBundleCorrupt {
            task_id: expected_task_id,
            path: bundle_dir.to_string_lossy().into_owned(),
            reason: other.to_string(),
        },
    })
}

fn read_envelope_for_id(
    bundle_dir: &Path,
    expected_task_id: &str,
) -> Result<TaskEnvelopeV2, OrbitError> {
    let envelope_path = bundle_dir.join(TASK_ENVELOPE_FILE_NAME);
    if !envelope_path.is_file() {
        return Err(OrbitError::not_found(
            NotFoundKind::Task,
            expected_task_id.to_string(),
        ));
    }
    let envelope: TaskEnvelopeV2 =
        read_migrated_yaml_file(&envelope_path, task_migrations::envelope_plan())?;
    if envelope.id != expected_task_id {
        return Err(OrbitError::Store(format!(
            "task bundle directory {} represents task id {} but contains task id {}",
            bundle_dir.display(),
            expected_task_id,
            envelope.id
        )));
    }
    Ok(envelope)
}

fn read_bundle_for_id(
    bundle_dir: &Path,
    expected_task_id: &str,
    payloads: ArtifactPayloadCheck,
) -> Result<TaskBundleV2, OrbitError> {
    let mut bundle = TaskBundleV2 {
        envelope: read_envelope_for_id(bundle_dir, expected_task_id)?,
        description: read_required_text(&bundle_dir.join(TASK_DESCRIPTION_FILE_NAME))?,
        acceptance: read_required_text(&bundle_dir.join(TASK_ACCEPTANCE_FILE_NAME))?,
        plan: read_required_text(&bundle_dir.join(TASK_PLAN_FILE_NAME))?,
        execution_summary: read_required_text(&bundle_dir.join(TASK_EXECUTION_SUMMARY_FILE_NAME))?,
        events: read_task_events(&bundle_dir.join(TASK_EVENTS_FILE_NAME))?,
        comments: read_task_comments(&bundle_dir.join(TASK_COMMENTS_FILE_NAME))?,
        artifact_manifest: read_artifact_manifest(bundle_dir, payloads)?,
    };
    commit::apply_pending_read_view(bundle_dir, &mut bundle)?;
    validate_bundle(&bundle)?;
    Ok(bundle)
}

fn validate_bundle(bundle: &TaskBundleV2) -> Result<(), OrbitError> {
    bundle.envelope.validate()?;
    for event in &bundle.events {
        event.validate()?;
    }
    for comment in &bundle.comments {
        comment.validate()?;
    }
    if let Some(manifest) = &bundle.artifact_manifest {
        manifest.validate()?;
    }
    validate_bundle_consistency(bundle)?;
    Ok(())
}

fn validate_bundle_consistency(bundle: &TaskBundleV2) -> Result<(), OrbitError> {
    if let Some(last_status) = bundle.events.iter().rev().find_map(|event| event.to_status)
        && last_status != bundle.envelope.status
    {
        return Err(OrbitError::Store(format!(
            "task event log status '{}' does not match envelope status '{}' for {}",
            last_status, bundle.envelope.status, bundle.envelope.id
        )));
    }
    Ok(())
}

fn ensure_bundle_dirs(bundle_dir: &Path) -> Result<(), OrbitError> {
    fs::create_dir_all(bundle_dir).map_err(|err| OrbitError::from_write_io(bundle_dir, err))?;
    let artifact_files_dir = bundle_dir
        .join(TASK_ARTIFACTS_DIR_NAME)
        .join(TASK_ARTIFACT_FILES_DIR_NAME);
    fs::create_dir_all(&artifact_files_dir)
        .map_err(|err| OrbitError::from_write_io(&artifact_files_dir, err))?;
    Ok(())
}

fn validate_bundle_dir_matches_task_id(bundle_dir: &Path, task_id: &str) -> Result<(), OrbitError> {
    let name = task_id_from_bundle_dir(bundle_dir)?;
    if name != task_id {
        return Err(OrbitError::Store(format!(
            "task bundle directory {} does not match task id {}",
            bundle_dir.display(),
            task_id
        )));
    }
    Ok(())
}

fn task_id_from_bundle_dir(bundle_dir: &Path) -> Result<String, OrbitError> {
    bundle_dir
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::Store(format!("invalid task bundle path {}", bundle_dir.display()))
        })
}

fn read_migrated_yaml_file<T>(path: &Path, plan: &Plan) -> Result<T, OrbitError>
where
    T: DeserializeOwned,
{
    let raw = read_required_text(path)?;
    let value: serde_yaml::Value = parse_yaml_with(&raw, path, |_, err| {
        OrbitError::Store(format!("invalid YAML at {}: {err}", path.display()))
    })?;
    let migrated = plan.migrate(value).map_err(|err| match err {
        OrbitError::Migration(msg) => {
            OrbitError::Migration(format!("{} ({})", msg, path.display()))
        }
        other => other,
    })?;
    serde_yaml::from_value(migrated)
        .map_err(|err| OrbitError::Store(format!("invalid YAML at {}: {err}", path.display())))
}

pub(crate) fn read_required_text(path: &Path) -> Result<String, OrbitError> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(OrbitError::Store(format!(
            "missing task bundle file {}",
            path.display()
        ))),
        Err(err) => Err(OrbitError::Io(err.to_string())),
    }
}

fn write_jsonl_file<T>(path: &Path, rows: &[T]) -> Result<(), OrbitError>
where
    T: serde::Serialize,
{
    let mut content = String::new();
    for row in rows {
        content.push_str(
            &serde_json::to_string(row).map_err(|err| OrbitError::Store(err.to_string()))?,
        );
        content.push('\n');
    }
    atomic_write_text(path, &content).map_err(|err| OrbitError::from_write_io(path, err))
}

fn read_task_events(path: &Path) -> Result<Vec<TaskEventRowV2>, OrbitError> {
    let events: Vec<TaskEventRowV2> = read_jsonl_file(path)?;
    for event in &events {
        event.validate()?;
    }
    Ok(events)
}

fn read_task_comments(path: &Path) -> Result<Vec<TaskCommentRowV2>, OrbitError> {
    let comments: Vec<TaskCommentRowV2> = read_jsonl_file(path)?;
    for comment in &comments {
        comment.validate()?;
    }
    Ok(comments)
}

fn read_jsonl_file<T>(path: &Path) -> Result<Vec<T>, OrbitError>
where
    T: DeserializeOwned,
{
    let raw = read_required_text(path)?;
    scan_jsonl_records(path, &raw)
}

pub(crate) fn append_jsonl_row<T>(path: &Path, row: &T) -> Result<(), OrbitError>
where
    T: serde::Serialize,
{
    let encoded = serde_json::to_string(row).map_err(|err| OrbitError::Store(err.to_string()))?;
    with_exclusive_file_lock(path, "task jsonl", || {
        repair_jsonl_tail(path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.write_all(encoded.as_bytes())
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.write_all(b"\n")
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.flush()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.sync_all()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        Ok(())
    })
}

fn repair_jsonl_tail(path: &Path) -> Result<(), OrbitError> {
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(path) else {
        return Ok(());
    };

    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(OrbitError::from)?;
    let scan = scan_jsonl_tail(path, &raw)?;
    if scan.truncate_at < raw.len() as u64 {
        file.set_len(scan.truncate_at)
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.seek(SeekFrom::End(0))
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.sync_all()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
    }
    Ok(())
}

fn scan_jsonl_records<T>(path: &Path, raw: &str) -> Result<Vec<T>, OrbitError>
where
    T: DeserializeOwned,
{
    let scan = scan_jsonl_tail(path, raw)?;
    let valid = &raw[..scan.truncate_at as usize];
    valid
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|err| {
                OrbitError::Store(format!("invalid JSONL row at {}: {err}", path.display()))
            })
        })
        .collect()
}

struct JsonlTailScan {
    truncate_at: u64,
}

fn scan_jsonl_tail(path: &Path, raw: &str) -> Result<JsonlTailScan, OrbitError> {
    if raw.is_empty() {
        return Ok(JsonlTailScan { truncate_at: 0 });
    }

    let mut offset = 0usize;
    let mut last_good = 0usize;
    for chunk in raw.split_inclusive('\n') {
        let next_offset = offset + chunk.len();
        if !chunk.ends_with('\n') {
            return Ok(JsonlTailScan {
                truncate_at: last_good as u64,
            });
        }

        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if line.trim().is_empty() {
            return Err(OrbitError::Store(format!(
                "blank JSONL row before tail at {}",
                path.display()
            )));
        }
        if let Err(err) = serde_json::from_str::<serde_json::Value>(line) {
            if next_offset == raw.len() {
                return Ok(JsonlTailScan {
                    truncate_at: last_good as u64,
                });
            }
            return Err(OrbitError::Store(format!(
                "invalid JSONL row before tail at {}: {err}",
                path.display()
            )));
        }

        last_good = next_offset;
        offset = next_offset;
    }

    Ok(JsonlTailScan {
        truncate_at: last_good as u64,
    })
}

fn read_artifact_manifest(
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
    fs::create_dir_all(&dest_files_dir)
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

fn cleanup_partial_bundle(bundle_dir: &Path) -> Result<(), OrbitError> {
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

#[cfg(test)]
mod tests;
