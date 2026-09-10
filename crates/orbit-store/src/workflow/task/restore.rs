//! Fail-closed, same-authority recovery from a validated task publication.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use crate::driver::file::task_bundle::{
    read_bundle_at, write_bundle_at, write_bundle_with_artifacts_at,
};
use crate::driver::sqlite::task_registry::{TaskRegistryStore, parse_orb_task_number};

use super::inspect::{ValidatedPublicationBundle, load_validated_publication};
use super::{OmittedAttachment, PublicationInspectRequest};

const RESTORE_LABEL: &str = "publication restore";

/// Destination policy for same-authority recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationRestoreMode {
    /// Refuse any destination that already contains a canonical task.
    EmptyDestination,
    /// Permit existing ids only when their full canonical bundle content is
    /// identical to the publication. Missing ids are restored normally.
    AllowIdenticalRetry,
}

/// Whether the publication contained every task attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationRecoveryCompleteness {
    Complete,
    IncompleteAttachments,
}

/// Pairing inputs and the explicitly selected destination policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationRestoreRequest {
    /// Existing task-registry partition that receives restored bundles.
    pub task_workspace_id: String,
    pub publication: PublicationInspectRequest,
    pub mode: PublicationRestoreMode,
}

/// Result of a committed restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationRestoreOutcome {
    pub workspace_id: String,
    pub publication_id: String,
    pub generation: u64,
    pub restored_task_ids: Vec<String>,
    pub already_present_task_ids: Vec<String>,
    pub omitted_attachments: Vec<OmittedAttachment>,
    pub completeness: PublicationRecoveryCompleteness,
}

/// Restore a task publication without renumbering, implicit identity adoption,
/// or partial canonical mutation.
pub fn restore_publication(
    registry: &TaskRegistryStore,
    request: PublicationRestoreRequest,
) -> Result<PublicationRestoreOutcome, OrbitError> {
    restore_publication_inner(registry, request, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestoreFailurePoint {
    BundlePublication,
    IndexRebuild,
    AllocatorAdvance,
}

#[cfg(test)]
pub(super) fn restore_publication_with_failure(
    registry: &TaskRegistryStore,
    request: PublicationRestoreRequest,
    failure: RestoreFailurePoint,
) -> Result<PublicationRestoreOutcome, OrbitError> {
    restore_publication_inner(registry, request, Some(failure))
}

fn restore_publication_inner(
    registry: &TaskRegistryStore,
    request: PublicationRestoreRequest,
    failure: Option<RestoreFailurePoint>,
) -> Result<PublicationRestoreOutcome, OrbitError> {
    // The inspector owns repository fetch, branch/commit lineage, envelope
    // pairing, schema support, bundle validation, JSONL validation, omission
    // validation, and attachment checksum verification. Recovery consumes that
    // exact result rather than recreating a second validation path.
    let validated = load_validated_publication(request.publication.clone())?;
    let envelope = &validated.inspection.envelope;
    let task_workspace_id = request.task_workspace_id.clone();
    assert_destination_pairing(registry, &request)?;

    let existing = registry.tasks_for_workspace(&task_workspace_id)?;
    let destination_has_entries = canonical_destination_has_entries(
        registry.workspaces_dir().join(&task_workspace_id).as_path(),
    )?;
    if request.mode == PublicationRestoreMode::EmptyDestination
        && (!existing.is_empty() || destination_has_entries)
    {
        return Err(restore_error(format!(
            "task workspace '{task_workspace_id}' is not an empty restore destination"
        )));
    }

    let previous_envelopes = existing
        .iter()
        .map(|binding| read_bundle_at(&binding.canonical_path).map(|bundle| bundle.envelope))
        .collect::<Result<Vec<_>, _>>()?;

    let mut missing = Vec::new();
    let mut already_present = Vec::new();
    for published in &validated.bundles {
        let task_id = &published.bundle.envelope.id;
        match registry.find_task_binding(task_id)? {
            None => {
                let destination =
                    registry.canonical_task_bundle_path(&task_workspace_id, task_id)?;
                if destination.exists() {
                    return Err(restore_error(format!(
                        "canonical path for task '{task_id}' exists without a registry binding"
                    )));
                }
                missing.push(published);
            }
            Some(binding) => {
                let identical = binding.workspace_id == task_workspace_id
                    && read_bundle_at(&binding.canonical_path)
                        .is_ok_and(|bundle| bundle == published.bundle);
                if request.mode != PublicationRestoreMode::AllowIdenticalRetry || !identical {
                    return Err(restore_error(format!(
                        "task id '{task_id}' collides with non-identical canonical content"
                    )));
                }
                already_present.push(task_id.clone());
            }
        }
    }

    if missing.is_empty() {
        return Ok(outcome(envelope, Vec::new(), already_present));
    }

    let workspace_root = registry.workspaces_dir().join(&task_workspace_id);
    fs::create_dir_all(&workspace_root)
        .map_err(|error| OrbitError::from_write_io(&workspace_root, error))?;
    let staging = tempfile::Builder::new()
        .prefix(".orbit-publication-restore-")
        .tempdir_in(&workspace_root)
        .map_err(|error| OrbitError::from_write_io(&workspace_root, error))?;
    for published in &missing {
        stage_bundle(staging.path(), published)?;
    }

    let previous_allocator = registry.allocator_next_number()?;
    let restored_ids = missing
        .iter()
        .map(|published| published.bundle.envelope.id.clone())
        .collect::<Vec<_>>();
    let mut guard = RestoreGuard::new(
        registry,
        task_workspace_id.clone(),
        previous_envelopes,
        previous_allocator,
    );

    for (index, published) in missing.iter().enumerate() {
        let task_id = &published.bundle.envelope.id;
        let source = staging.path().join(task_id);
        let destination = registry.canonical_task_bundle_path(&task_workspace_id, task_id)?;
        fs::rename(&source, &destination)
            .map_err(|error| OrbitError::from_write_io(&destination, error))?;
        guard.published_dirs.push(destination);
        if index == 0 {
            inject(failure, RestoreFailurePoint::BundlePublication)?;
        }
    }

    for task_id in &restored_ids {
        let path = registry.canonical_task_bundle_path(&task_workspace_id, task_id)?;
        registry.register_task_bundle(task_id, &task_workspace_id, &path)?;
        guard.registered_ids.push(task_id.clone());
    }

    rebuild_workspace_index(registry, &task_workspace_id)?;
    inject(failure, RestoreFailurePoint::IndexRebuild)?;

    let target_allocator = restored_ids
        .iter()
        .filter_map(|task_id| parse_orb_task_number(task_id))
        .max()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| restore_error("publication contains no allocatable task id"))?
        .max(previous_allocator);
    registry.bump_allocator_to_at_least(target_allocator)?;
    guard.advanced_allocator = Some(target_allocator);
    inject(failure, RestoreFailurePoint::AllocatorAdvance)?;

    guard.commit();
    Ok(outcome(envelope, restored_ids, already_present))
}

fn assert_destination_pairing(
    registry: &TaskRegistryStore,
    request: &PublicationRestoreRequest,
) -> Result<(), OrbitError> {
    let binding = registry
        .find_workspace_binding(&request.task_workspace_id)?
        .ok_or_else(|| {
            restore_error(format!(
                "task workspace '{}' is not registered",
                request.task_workspace_id
            ))
        })?;
    if binding.workspace_id != request.task_workspace_id {
        return Err(restore_error(
            "task workspace selector resolved to another workspace",
        ));
    }
    match binding.repo_fingerprint.as_deref() {
        Some(fingerprint) if fingerprint == request.publication.source_repository_fingerprint => {
            Ok(())
        }
        Some(fingerprint) => Err(restore_error(format!(
            "source repository fingerprint mismatch: destination has '{fingerprint}', publication expects '{}'",
            request.publication.source_repository_fingerprint
        ))),
        None => Err(restore_error(format!(
            "task workspace '{}' has no registered source repository fingerprint; restore never adopts one implicitly",
            request.task_workspace_id
        ))),
    }
}

fn canonical_destination_has_entries(workspace_root: &Path) -> Result<bool, OrbitError> {
    let mut entries = match fs::read_dir(workspace_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(OrbitError::from_write_io(workspace_root, error)),
    };
    entries
        .next()
        .transpose()
        .map(|entry| entry.is_some())
        .map_err(|error| OrbitError::from_write_io(workspace_root, error))
}

fn stage_bundle(root: &Path, published: &ValidatedPublicationBundle) -> Result<(), OrbitError> {
    let target = root.join(&published.bundle.envelope.id);
    if published
        .bundle
        .artifact_manifest
        .as_ref()
        .is_some_and(|manifest| !manifest.files.is_empty())
    {
        write_bundle_with_artifacts_at(&target, &published.bundle, &published.source_dir)
    } else {
        write_bundle_at(&target, &published.bundle)
    }
}

fn rebuild_workspace_index(
    registry: &TaskRegistryStore,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let envelopes = registry
        .tasks_for_workspace(workspace_id)?
        .into_iter()
        .map(|binding| read_bundle_at(&binding.canonical_path).map(|bundle| bundle.envelope))
        .collect::<Result<Vec<_>, _>>()?;
    registry.replace_workspace_task_indexes(workspace_id, &envelopes)
}

fn outcome(
    envelope: &super::PublicationEnvelope,
    restored_task_ids: Vec<String>,
    already_present_task_ids: Vec<String>,
) -> PublicationRestoreOutcome {
    let completeness = if envelope.omitted_attachments.is_empty() {
        PublicationRecoveryCompleteness::Complete
    } else {
        PublicationRecoveryCompleteness::IncompleteAttachments
    };
    PublicationRestoreOutcome {
        workspace_id: envelope.workspace_id.clone(),
        publication_id: envelope.publication_id.clone(),
        generation: envelope.generation,
        restored_task_ids,
        already_present_task_ids,
        omitted_attachments: envelope.omitted_attachments.clone(),
        completeness,
    }
}

fn inject(
    selected: Option<RestoreFailurePoint>,
    current: RestoreFailurePoint,
) -> Result<(), OrbitError> {
    if selected == Some(current) {
        return Err(restore_error(format!("injected failure after {current:?}")));
    }
    Ok(())
}

struct RestoreGuard<'a> {
    registry: &'a TaskRegistryStore,
    workspace_id: String,
    previous_envelopes: Vec<orbit_types::task::TaskEnvelopeV2>,
    previous_allocator: u32,
    advanced_allocator: Option<u32>,
    published_dirs: Vec<PathBuf>,
    registered_ids: Vec<String>,
    armed: bool,
}

impl<'a> RestoreGuard<'a> {
    fn new(
        registry: &'a TaskRegistryStore,
        workspace_id: String,
        previous_envelopes: Vec<orbit_types::task::TaskEnvelopeV2>,
        previous_allocator: u32,
    ) -> Self {
        Self {
            registry,
            workspace_id,
            previous_envelopes,
            previous_allocator,
            advanced_allocator: None,
            published_dirs: Vec::new(),
            registered_ids: Vec::new(),
            armed: true,
        }
    }

    fn commit(&mut self) {
        self.armed = false;
    }

    fn rollback(&mut self) {
        for task_id in self.registered_ids.iter().rev() {
            let _ = self
                .registry
                .unregister_task_bundle(task_id, &self.workspace_id);
        }
        let _ = self
            .registry
            .replace_workspace_task_indexes(&self.workspace_id, &self.previous_envelopes);
        for dir in self.published_dirs.iter().rev() {
            let _ = fs::remove_dir_all(dir);
        }
        if let Some(current) = self.advanced_allocator {
            let _ = self
                .registry
                .restore_allocator_after_failed_restore(current, self.previous_allocator);
        }
        self.armed = false;
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}

fn restore_error(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(format!("{RESTORE_LABEL}: {}", message.into()))
}
