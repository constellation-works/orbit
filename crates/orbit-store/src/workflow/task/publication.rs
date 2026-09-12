//! Deterministic, validated task-publication snapshot construction.
//!
//! This module owns only the filesystem projection. Git transport, publication
//! authority checks, and last-success recording are separate workflow phases.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{validate_machine_id, validate_registry_identifier};
use orbit_types::policy::{compile_glob_regex, match_glob};
use orbit_types::task::{
    ArtifactManifestFileV2, TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME,
    TASK_COMMENTS_FILE_NAME, TASK_EVENTS_FILE_NAME, validate_orb_task_id,
    validate_relative_artifact_path,
};
use orbit_types::workspace::{validate_git_commit_id, validate_source_repository_fingerprint};
use serde::{Deserialize, Serialize};

use crate::driver::file::task_bundle::{
    TaskBundleV2, read_bundle_at, write_bundle_at, write_bundle_with_artifacts_at,
};
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::fs::yaml::write_yaml_atomic_with;

/// Version of the publication envelope and tree contract.
pub const TASK_PUBLICATION_FORMAT_VERSION: u32 = 1;
/// Root envelope name in a publication snapshot.
pub const PUBLICATION_ENVELOPE_FILE_NAME: &str = "orbit-task-publication.yaml";
/// Root directory holding canonical task projections.
pub const PUBLICATION_TASKS_DIR_NAME: &str = "tasks";

/// Attachment projection recorded in the publication envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentPolicyKind {
    /// Reject a workspace that contains any attachment.
    Fail,
    /// Validate and copy admitted attachment bytes.
    Include,
    /// Remove attachment manifests and blobs while recording an omission ledger.
    Omit,
}

/// Behavior when a configured sensitivity scanner cannot produce a verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScannerFailureBehavior {
    /// Reject publication when the scanner is missing or fails.
    Reject,
    /// Continue after path and size checks, explicitly accepting an unchecked file.
    AllowUnchecked,
}

/// Attachment controls applied independently of task-artifact upload limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPolicy {
    pub kind: AttachmentPolicyKind,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub deny_patterns: Vec<String>,
    pub scanner_failure_behavior: ScannerFailureBehavior,
}

impl AttachmentPolicy {
    fn validate(&self) -> Result<(), OrbitError> {
        for pattern in &self.deny_patterns {
            if pattern.trim() != pattern || pattern.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "attachment deny patterns must be non-empty and trimmed".to_string(),
                ));
            }
            compile_glob_regex(pattern).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "invalid attachment deny pattern '{pattern}': {error}"
                ))
            })?;
        }
        Ok(())
    }
}

/// Caller-supplied publication identity and commit metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationSnapshotMetadata {
    pub publication_id: String,
    pub workspace_id: String,
    pub source_repository_fingerprint: String,
    pub authority_machine_id: String,
    pub generation: u64,
    pub published_at: DateTime<Utc>,
    pub previous_publication: Option<String>,
}

impl PublicationSnapshotMetadata {
    fn validate(&self) -> Result<(), OrbitError> {
        validate_registry_identifier("publication_id", &self.publication_id)
            .map_err(invalid_identity)?;
        validate_registry_identifier("workspace_id", &self.workspace_id)
            .map_err(invalid_identity)?;
        validate_source_repository_fingerprint(&self.source_repository_fingerprint)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        validate_machine_id(&self.authority_machine_id).map_err(invalid_identity)?;
        if self.generation == 0 {
            return Err(OrbitError::InvalidInput(
                "publication generation must be at least 1".to_string(),
            ));
        }
        if let Some(commit) = self.previous_publication.as_deref() {
            validate_git_commit_id(commit)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        }
        Ok(())
    }
}

/// A deliberately content-free record of an attachment excluded by `omit`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmittedAttachment {
    pub task_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

impl OmittedAttachment {
    fn validate(&self) -> Result<(), OrbitError> {
        validate_orb_task_id(&self.task_id)?;
        validate_relative_artifact_path(&self.path)?;
        if !is_sha256_hex(&self.sha256) {
            return Err(OrbitError::InvalidInput(format!(
                "omitted attachment '{}' for task '{}' has an invalid sha256",
                self.path, self.task_id
            )));
        }
        Ok(())
    }
}

/// Versioned root document for one immutable publication snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationEnvelope {
    pub format_version: u32,
    pub publication_id: String,
    pub workspace_id: String,
    pub source_repository_fingerprint: String,
    pub authority_machine_id: String,
    pub generation: u64,
    pub published_at: DateTime<Utc>,
    pub task_schema_version: u32,
    pub previous_publication: Option<String>,
    pub attachment_policy: AttachmentPolicyKind,
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub omitted_attachments: Vec<OmittedAttachment>,
}

impl PublicationEnvelope {
    /// Validate schema support, identity fields, ordering, and projection consistency.
    pub fn validate(&self) -> Result<(), OrbitError> {
        if self.format_version != TASK_PUBLICATION_FORMAT_VERSION {
            return Err(OrbitError::InvalidInput(format!(
                "unsupported task publication format version {}; supported version is {}",
                self.format_version, TASK_PUBLICATION_FORMAT_VERSION
            )));
        }
        if self.task_schema_version != TASK_ARTIFACT_SCHEMA_VERSION {
            return Err(OrbitError::InvalidInput(format!(
                "unsupported task schema version {}; supported version is {}",
                self.task_schema_version, TASK_ARTIFACT_SCHEMA_VERSION
            )));
        }
        PublicationSnapshotMetadata {
            publication_id: self.publication_id.clone(),
            workspace_id: self.workspace_id.clone(),
            source_repository_fingerprint: self.source_repository_fingerprint.clone(),
            authority_machine_id: self.authority_machine_id.clone(),
            generation: self.generation,
            published_at: self.published_at,
            previous_publication: self.previous_publication.clone(),
        }
        .validate()?;

        validate_sorted_unique_task_ids(&self.task_ids)?;
        if !self.omitted_attachments.is_sorted() {
            return Err(OrbitError::InvalidInput(
                "publication omitted_attachments must be sorted".to_string(),
            ));
        }
        let task_ids: BTreeSet<&str> = self.task_ids.iter().map(String::as_str).collect();
        let mut omission_keys = BTreeSet::new();
        for omitted in &self.omitted_attachments {
            omitted.validate()?;
            if !task_ids.contains(omitted.task_id.as_str()) {
                return Err(OrbitError::InvalidInput(format!(
                    "omitted attachment '{}' names unpublished task '{}'",
                    omitted.path, omitted.task_id
                )));
            }
            if !omission_keys.insert((omitted.task_id.as_str(), omitted.path.as_str())) {
                return Err(OrbitError::InvalidInput(format!(
                    "duplicate omitted attachment '{}' for task '{}'",
                    omitted.path, omitted.task_id
                )));
            }
        }
        if self.attachment_policy != AttachmentPolicyKind::Omit
            && !self.omitted_attachments.is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "only the omit attachment policy may record omitted attachments".to_string(),
            ));
        }
        Ok(())
    }

    /// Deserialize and validate a publication envelope.
    pub fn from_yaml(raw: &str) -> Result<Self, OrbitError> {
        let envelope: Self = serde_yaml::from_str(raw).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid task publication envelope: {error}"))
        })?;
        envelope.validate()?;
        Ok(envelope)
    }

    /// Serialize a validated publication envelope in canonical field order.
    pub fn to_yaml(&self) -> Result<String, OrbitError> {
        self.validate()?;
        serde_yaml::to_string(self).map_err(|error| OrbitError::Store(error.to_string()))
    }
}

/// A scanner input whose bytes are never retained in the publication envelope.
pub struct AttachmentScanInput<'a> {
    pub task_id: &'a str,
    pub path: &'a str,
    pub media_type: &'a str,
    pub bytes: &'a [u8],
}

/// Sensitivity verdict for one attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentScanOutcome {
    Clear,
    Sensitive,
}

/// Content-free scanner failure classes safe to surface in policy errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentScanFailure {
    Unavailable,
    Failed,
}

/// Pluggable sensitivity boundary used by `include` publication.
pub trait AttachmentSensitivityScanner {
    fn scan(
        &self,
        input: AttachmentScanInput<'_>,
    ) -> Result<AttachmentScanOutcome, AttachmentScanFailure>;
}

/// Result of a successfully published snapshot tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationSnapshotOutcome {
    pub destination: PathBuf,
    pub envelope: PublicationEnvelope,
    pub included_attachment_bytes: u64,
    pub omitted_attachment_bytes: u64,
}

/// Build a validated publication tree into a caller-supplied empty destination.
///
/// Every canonical bundle is validated before its projection is staged. The
/// destination path appears only after the envelope and all task trees have
/// been completed successfully.
pub fn build_publication_snapshot(
    registry: &TaskRegistryStore,
    destination: &Path,
    metadata: PublicationSnapshotMetadata,
    policy: &AttachmentPolicy,
    scanner: Option<&dyn AttachmentSensitivityScanner>,
) -> Result<PublicationSnapshotOutcome, OrbitError> {
    let task_workspace_id = metadata.workspace_id.clone();
    build_publication_snapshot_from_task_workspace(
        registry,
        destination,
        metadata,
        &task_workspace_id,
        policy,
        scanner,
    )
}

/// Build a snapshot carrying logical publication metadata from a distinct
/// canonical task-registry partition.
pub(crate) fn build_publication_snapshot_from_task_workspace(
    registry: &TaskRegistryStore,
    destination: &Path,
    metadata: PublicationSnapshotMetadata,
    task_workspace_id: &str,
    policy: &AttachmentPolicy,
    scanner: Option<&dyn AttachmentSensitivityScanner>,
) -> Result<PublicationSnapshotOutcome, OrbitError> {
    metadata.validate()?;
    validate_registry_identifier("task_workspace_id", task_workspace_id)
        .map_err(invalid_identity)?;
    policy.validate()?;
    if destination.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "task publication destination already exists: {}",
            destination.display()
        )));
    }
    let parent = destination.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "task publication destination has no parent: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| OrbitError::from_write_io(parent, error))?;

    let binding = registry
        .find_workspace_binding(task_workspace_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "task workspace '{task_workspace_id}' is not registered in the coordination registry"
            ))
        })?;
    if binding.partition_id != task_workspace_id {
        return Err(OrbitError::InvalidInput(format!(
            "task workspace selector '{task_workspace_id}' resolved to unexpected workspace '{}'",
            binding.partition_id
        )));
    }

    let mut task_ids: Vec<String> = registry
        .tasks_for_workspace(task_workspace_id)?
        .into_iter()
        .map(|task| task.task_id)
        .collect();
    task_ids.sort();
    task_ids.dedup();
    validate_sorted_unique_task_ids(&task_ids)?;

    let staging = tempfile::Builder::new()
        .prefix(".orbit-task-publication-")
        .tempdir_in(parent)
        .map_err(|error| OrbitError::from_write_io(parent, error))?;
    let tasks_root = staging.path().join(PUBLICATION_TASKS_DIR_NAME);
    fs::create_dir(&tasks_root).map_err(|error| OrbitError::from_write_io(&tasks_root, error))?;

    let mut omitted_attachments = Vec::new();
    let mut included_attachment_bytes = 0u64;
    let mut omitted_attachment_bytes = 0u64;
    for task_id in &task_ids {
        let source = registry.canonical_task_bundle_path(task_workspace_id, task_id)?;
        validate_jsonl_files(task_id, &source)?;
        let mut bundle = read_bundle_at(&source).map_err(|error| {
            OrbitError::Store(format!(
                "task publication validation failed for task '{task_id}' at '{}': {error}",
                source.display()
            ))
        })?;
        let files = sorted_manifest_files(&bundle);
        validate_manifest_uniqueness(task_id, &files)?;
        apply_attachment_policy(
            task_id,
            &source,
            &files,
            policy,
            scanner,
            &mut included_attachment_bytes,
            &mut omitted_attachment_bytes,
            &mut omitted_attachments,
        )?;

        if let Some(manifest) = &mut bundle.artifact_manifest {
            manifest.files = files;
        }
        if policy.kind == AttachmentPolicyKind::Omit {
            bundle.artifact_manifest = None;
        }
        let target = tasks_root.join(task_id);
        if policy.kind == AttachmentPolicyKind::Include && bundle.artifact_manifest.is_some() {
            write_bundle_with_artifacts_at(&target, &bundle, &source)?;
        } else {
            write_bundle_at(&target, &bundle)?;
        }
    }

    omitted_attachments.sort();
    let envelope = PublicationEnvelope {
        format_version: TASK_PUBLICATION_FORMAT_VERSION,
        publication_id: metadata.publication_id,
        workspace_id: metadata.workspace_id,
        source_repository_fingerprint: metadata.source_repository_fingerprint,
        authority_machine_id: metadata.authority_machine_id,
        generation: metadata.generation,
        published_at: metadata.published_at,
        task_schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        previous_publication: metadata.previous_publication,
        attachment_policy: policy.kind,
        task_ids,
        omitted_attachments,
    };
    envelope.validate()?;
    write_yaml_atomic_with(
        &staging.path().join(PUBLICATION_ENVELOPE_FILE_NAME),
        &envelope,
        |error| OrbitError::Store(format!("failed to encode publication envelope: {error}")),
    )?;

    if destination.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "task publication destination appeared during construction: {}",
            destination.display()
        )));
    }
    fs::rename(staging.path(), destination)
        .map_err(|error| OrbitError::from_write_io(destination, error))?;

    Ok(PublicationSnapshotOutcome {
        destination: destination.to_path_buf(),
        envelope,
        included_attachment_bytes,
        omitted_attachment_bytes,
    })
}

fn validate_sorted_unique_task_ids(task_ids: &[String]) -> Result<(), OrbitError> {
    let mut previous: Option<&str> = None;
    for task_id in task_ids {
        validate_orb_task_id(task_id)?;
        if previous.is_some_and(|prior| prior >= task_id.as_str()) {
            return Err(OrbitError::InvalidInput(
                "publication task_ids must be sorted and unique".to_string(),
            ));
        }
        previous = Some(task_id);
    }
    Ok(())
}

fn sorted_manifest_files(bundle: &TaskBundleV2) -> Vec<ArtifactManifestFileV2> {
    let mut files = bundle
        .artifact_manifest
        .as_ref()
        .map(|manifest| manifest.files.clone())
        .unwrap_or_default();
    files.sort_by(|left, right| {
        (&left.path, &left.blob, &left.sha256).cmp(&(&right.path, &right.blob, &right.sha256))
    });
    files
}

fn validate_manifest_uniqueness(
    task_id: &str,
    files: &[ArtifactManifestFileV2],
) -> Result<(), OrbitError> {
    let mut paths = BTreeSet::new();
    let mut blobs = BTreeSet::new();
    for file in files {
        if !paths.insert(file.path.as_str()) {
            return Err(policy_error(
                task_id,
                &file.path,
                "duplicate logical attachment path",
            ));
        }
        if !blobs.insert(file.blob.as_str()) {
            return Err(policy_error(
                task_id,
                &file.path,
                "duplicate attachment blob path",
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_attachment_policy(
    task_id: &str,
    source: &Path,
    files: &[ArtifactManifestFileV2],
    policy: &AttachmentPolicy,
    scanner: Option<&dyn AttachmentSensitivityScanner>,
    included_bytes: &mut u64,
    omitted_bytes: &mut u64,
    omitted: &mut Vec<OmittedAttachment>,
) -> Result<(), OrbitError> {
    if policy.kind == AttachmentPolicyKind::Fail
        && let Some(file) = files.first()
    {
        return Err(policy_error(
            task_id,
            &file.path,
            "attachments are forbidden by the fail policy",
        ));
    }

    for file in files {
        if policy.kind == AttachmentPolicyKind::Omit {
            *omitted_bytes = omitted_bytes.checked_add(file.size_bytes).ok_or_else(|| {
                policy_error(
                    task_id,
                    &file.path,
                    "omitted attachment byte count overflow",
                )
            })?;
            omitted.push(OmittedAttachment {
                task_id: task_id.to_string(),
                path: file.path.clone(),
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            });
            continue;
        }
        if policy.kind != AttachmentPolicyKind::Include {
            continue;
        }
        if file.size_bytes > policy.max_file_bytes {
            return Err(policy_error(
                task_id,
                &file.path,
                "attachment exceeds the per-file publication limit",
            ));
        }
        let next_total = included_bytes.checked_add(file.size_bytes).ok_or_else(|| {
            policy_error(
                task_id,
                &file.path,
                "included attachment byte count overflow",
            )
        })?;
        if next_total > policy.max_total_bytes {
            return Err(policy_error(
                task_id,
                &file.path,
                "attachments exceed the total publication limit",
            ));
        }
        for pattern in &policy.deny_patterns {
            if match_glob(pattern, &file.path)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?
            {
                return Err(policy_error(
                    task_id,
                    &file.path,
                    "attachment path matches a publication deny pattern",
                ));
            }
        }
        let blob_path = source.join(TASK_ARTIFACTS_DIR_NAME).join(&file.blob);
        let bytes = fs::read(&blob_path).map_err(|error| {
            OrbitError::Store(format!(
                "task publication could not read attachment for task '{task_id}' at '{}': {error}",
                file.path
            ))
        })?;
        let scan_result = scanner.map(|scanner| {
            scanner.scan(AttachmentScanInput {
                task_id,
                path: &file.path,
                media_type: &file.media_type,
                bytes: &bytes,
            })
        });
        match scan_result {
            Some(Ok(AttachmentScanOutcome::Clear)) => {}
            Some(Ok(AttachmentScanOutcome::Sensitive)) => {
                return Err(policy_error(
                    task_id,
                    &file.path,
                    "attachment was classified as sensitive",
                ));
            }
            Some(Err(_)) | None
                if policy.scanner_failure_behavior == ScannerFailureBehavior::Reject =>
            {
                return Err(policy_error(
                    task_id,
                    &file.path,
                    "attachment sensitivity scanner did not produce a verdict",
                ));
            }
            Some(Err(_)) | None => {}
        }
        *included_bytes = next_total;
    }
    Ok(())
}

/// The Git parent is the commit-graph authority; `previous_publication`
/// duplicates it so a detached snapshot stays self-describing. They must agree.
pub(crate) fn assert_envelope_parent_lineage(
    envelope: &PublicationEnvelope,
    parent: Option<&str>,
    label: &str,
) -> Result<(), OrbitError> {
    match (envelope.previous_publication.as_deref(), parent) {
        (None, None) => Ok(()),
        (Some(previous), Some(parent)) if previous.eq_ignore_ascii_case(parent) => Ok(()),
        (previous, parent) => Err(OrbitError::InvalidInput(format!(
            "{label}: Git parent/previous-publication mismatch: envelope previous_publication is {}, Git parent is {}",
            previous.unwrap_or("null"),
            parent.unwrap_or("null")
        ))),
    }
}

pub(crate) fn validate_jsonl_files(task_id: &str, bundle_dir: &Path) -> Result<(), OrbitError> {
    for file_name in [TASK_EVENTS_FILE_NAME, TASK_COMMENTS_FILE_NAME] {
        let path = bundle_dir.join(file_name);
        let raw = fs::read_to_string(&path).map_err(|error| {
            OrbitError::Store(format!(
                "task publication could not read task '{task_id}' path '{file_name}': {error}"
            ))
        })?;
        if !raw.is_empty() && !raw.ends_with('\n') {
            return Err(OrbitError::Store(format!(
                "task publication rejected incomplete JSONL tail for task '{task_id}' at '{file_name}'"
            )));
        }
        for line in raw.lines() {
            if line.trim().is_empty() || serde_json::from_str::<serde_json::Value>(line).is_err() {
                return Err(OrbitError::Store(format!(
                    "task publication rejected invalid JSONL for task '{task_id}' at '{file_name}'"
                )));
            }
        }
    }
    Ok(())
}

fn policy_error(task_id: &str, path: &str, reason: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "task publication attachment '{path}' for task '{task_id}' was rejected: {reason}"
    ))
}

fn invalid_identity(error: orbit_types::identity::IdentityError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
