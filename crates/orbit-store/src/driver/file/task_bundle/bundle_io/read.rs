//! Bundle and envelope reads with migration and consistency validation.

use super::super::migrations as task_migrations;
use super::super::types::TaskBundleV2;
use crate::driver::file::task_bundle::bundle_io::artifacts::ArtifactPayloadCheck;
use crate::driver::file::task_bundle::bundle_io::artifacts::read_artifact_manifest;
use crate::driver::file::task_bundle::bundle_io::commit;
use crate::driver::file::task_bundle::bundle_io::jsonl::read_task_comments;
use crate::driver::file::task_bundle::bundle_io::jsonl::read_task_events;
use crate::fs::yaml::parse_yaml_with;
use orbit_common::migration::Plan;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    TASK_ACCEPTANCE_FILE_NAME, TASK_COMMENTS_FILE_NAME, TASK_DESCRIPTION_FILE_NAME,
    TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME, TASK_EXECUTION_SUMMARY_FILE_NAME,
    TASK_PLAN_FILE_NAME, TaskEnvelopeV2,
};
use serde::de::DeserializeOwned;
use std::fs::{self};
use std::path::Path;

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

pub(super) fn read_bundle_for_id(
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

pub(super) fn validate_bundle(bundle: &TaskBundleV2) -> Result<(), OrbitError> {
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

pub(super) fn validate_bundle_dir_matches_task_id(
    bundle_dir: &Path,
    task_id: &str,
) -> Result<(), OrbitError> {
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
