//! Read-only inspection of a task-publication Git repository.
//!
//! The consumer fetches a configured ordinary branch into an Orbit-owned cache,
//! validates pairing and snapshot integrity, and returns labelled task
//! projections. It never writes canonical tasks, allocators, checkout
//! projections, claims, audit rows, or execution records.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::io::create_private_dir_all;
use orbit_types::identity::{validate_machine_id, validate_registry_identifier};
use orbit_types::task::{TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACTS_DIR_NAME, TaskEnvelopeV2};
use orbit_types::workspace::{
    canonicalize_publication_branch, redact_git_remote, validate_git_commit_id,
    validate_source_repository_fingerprint,
};

use crate::driver::file::task_bundle::{TaskBundleV2, read_bundle_at};

use super::git::{GitRunner, field_mismatch, remote_has_password, remotes_match, short_branch};
use super::publication::{
    AttachmentPolicyKind, PUBLICATION_ENVELOPE_FILE_NAME, PUBLICATION_TASKS_DIR_NAME,
    PublicationEnvelope, assert_envelope_parent_lineage, validate_jsonl_files,
};

/// Error prefix and command label for every consumer-side failure.
const INSPECT_LABEL: &str = "publication inspect";

/// Caller-supplied pairing and fetch inputs. Identity is never inferred from
/// repository contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationInspectRequest {
    pub workspace_id: String,
    pub source_repository_fingerprint: String,
    pub publication_id: String,
    pub authority_machine_id: String,
    pub publication_remote: String,
    pub publication_branch: String,
    pub cache_dir: PathBuf,
    /// Inspect this commit when set; otherwise the configured branch tip.
    pub commit: Option<String>,
}

/// How a validated snapshot relates to the fetched branch tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationFreshness {
    Current,
    Stale,
}

/// Rendered results are publication snapshots, never live owner state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationRenderAuthority {
    Snapshot,
}

/// Identity and freshness attached to every inspected task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationInspectLabel {
    pub published_at: DateTime<Utc>,
    pub generation: u64,
    pub workspace_id: String,
    pub source_repository_fingerprint: String,
    pub authority_machine_id: String,
    pub publication_id: String,
    pub commit_id: String,
    pub incomplete_attachments: bool,
    pub freshness: PublicationFreshness,
    pub render_authority: PublicationRenderAuthority,
}

/// One validated task rendered from a publication snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedPublicationTask {
    pub label: PublicationInspectLabel,
    pub task: TaskEnvelopeV2,
    pub description: String,
    pub acceptance: String,
    pub plan: String,
    pub execution_summary: String,
}

/// A fully validated, labelled publication snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationInspection {
    pub label: PublicationInspectLabel,
    pub envelope: PublicationEnvelope,
    pub git_parent: Option<String>,
    pub tasks: Vec<InspectedPublicationTask>,
}

/// Fetch a publication branch into `request.cache_dir` and render labelled
/// read-only task state. Fail closed before returning any task when pairing,
/// schema, lineage, or content-integrity checks fail.
pub fn inspect_publication(
    request: PublicationInspectRequest,
) -> Result<PublicationInspection, OrbitError> {
    Ok(load_validated_publication(request)?.inspection)
}

/// Fully validated publication content retained for recovery. The public
/// inspection surface deliberately omits append streams and attachment bytes;
/// restore consumes this internal result so it cannot bypass the read-only
/// consumer contract or validate a subtly different projection.
pub(super) struct ValidatedPublicationSnapshot {
    pub inspection: PublicationInspection,
    pub bundles: Vec<ValidatedPublicationBundle>,
}

pub(super) struct ValidatedPublicationBundle {
    pub bundle: TaskBundleV2,
    pub source_dir: PathBuf,
}

pub(super) fn load_validated_publication(
    request: PublicationInspectRequest,
) -> Result<ValidatedPublicationSnapshot, OrbitError> {
    let request = validate_request(request)?;
    let fetched = fetch_publication(&request)?;
    let envelope = read_envelope(&fetched.tree_dir)?;
    assert_pairing(&request, &envelope, &fetched.branch)?;
    assert_envelope_parent_lineage(&envelope, fetched.git_parent.as_deref(), INSPECT_LABEL)?;
    let tasks = read_validated_tasks(&fetched.tree_dir, &envelope)?;
    let label = PublicationInspectLabel {
        published_at: envelope.published_at,
        generation: envelope.generation,
        workspace_id: envelope.workspace_id.clone(),
        source_repository_fingerprint: envelope.source_repository_fingerprint.clone(),
        authority_machine_id: envelope.authority_machine_id.clone(),
        publication_id: envelope.publication_id.clone(),
        commit_id: fetched.commit_id,
        incomplete_attachments: !envelope.omitted_attachments.is_empty(),
        freshness: fetched.freshness,
        render_authority: PublicationRenderAuthority::Snapshot,
    };
    let inspection = PublicationInspection {
        label: label.clone(),
        envelope,
        git_parent: fetched.git_parent,
        tasks: tasks
            .iter()
            .map(|task| InspectedPublicationTask {
                label: label.clone(),
                task: task.bundle.envelope.clone(),
                description: task.bundle.description.clone(),
                acceptance: task.bundle.acceptance.clone(),
                plan: task.bundle.plan.clone(),
                execution_summary: task.bundle.execution_summary.clone(),
            })
            .collect(),
    };
    Ok(ValidatedPublicationSnapshot {
        inspection,
        bundles: tasks,
    })
}

struct ValidatedRequest {
    workspace_id: String,
    source_repository_fingerprint: String,
    publication_id: String,
    authority_machine_id: String,
    publication_remote: String,
    publication_branch: String,
    cache_dir: PathBuf,
    commit: Option<String>,
}

struct FetchedSnapshot {
    tree_dir: PathBuf,
    branch: String,
    commit_id: String,
    git_parent: Option<String>,
    freshness: PublicationFreshness,
}

fn validate_request(request: PublicationInspectRequest) -> Result<ValidatedRequest, OrbitError> {
    validate_registry_identifier("workspace_id", &request.workspace_id).map_err(identity_error)?;
    validate_source_repository_fingerprint(&request.source_repository_fingerprint)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    validate_registry_identifier("publication_id", &request.publication_id)
        .map_err(identity_error)?;
    validate_machine_id(&request.authority_machine_id).map_err(identity_error)?;
    if request.publication_remote.trim().is_empty() {
        return Err(inspect_error("publication remote must not be empty"));
    }
    if remote_has_password(&request.publication_remote) {
        return Err(inspect_error(format!(
            "publication remote '{}' must not contain credentials",
            redact_git_remote(&request.publication_remote)
        )));
    }
    let publication_branch = canonicalize_publication_branch(&request.publication_branch)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    if request.cache_dir.as_os_str().is_empty() {
        return Err(inspect_error(
            "publication inspect cache directory is required",
        ));
    }
    let commit = match request.commit {
        Some(commit) => {
            validate_git_commit_id(&commit)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            Some(commit.to_ascii_lowercase())
        }
        None => None,
    };
    Ok(ValidatedRequest {
        workspace_id: request.workspace_id,
        source_repository_fingerprint: request.source_repository_fingerprint,
        publication_id: request.publication_id,
        authority_machine_id: request.authority_machine_id,
        publication_remote: request.publication_remote,
        publication_branch,
        cache_dir: request.cache_dir,
        commit,
    })
}

fn fetch_publication(request: &ValidatedRequest) -> Result<FetchedSnapshot, OrbitError> {
    let cache = request.cache_dir.join(&request.publication_id);
    let git_dir = cache.join("origin.git");
    let tree_dir = cache.join("tree");
    create_private_dir_all(&cache).map_err(|error| OrbitError::from_write_io(&cache, error))?;
    let git_dir_s = path_str(&git_dir)?;
    let tree_dir_s = path_str(&tree_dir)?;
    if git_dir.join("HEAD").is_file() {
        assert_cache_origin(&git_dir, &request.publication_remote)?;
        let refspec = format!(
            "{}:{}",
            request.publication_branch, request.publication_branch
        );
        git(&[
            "--git-dir",
            git_dir_s,
            "fetch",
            "--force",
            "origin",
            &refspec,
        ])?;
    } else {
        if git_dir.exists() {
            fs::remove_dir_all(&git_dir)
                .map_err(|error| OrbitError::from_write_io(&git_dir, error))?;
        }
        git(&[
            "clone",
            "--bare",
            "--single-branch",
            "--branch",
            short_branch(&request.publication_branch),
            "--",
            &request.publication_remote,
            git_dir_s,
        ])?;
    }

    let branch_tip = git(&[
        "--git-dir",
        git_dir_s,
        "rev-parse",
        &request.publication_branch,
    ])?
    .to_ascii_lowercase();
    let commit_id = request.commit.clone().unwrap_or_else(|| branch_tip.clone());
    if commit_id != branch_tip
        && git(&[
            "--git-dir",
            git_dir_s,
            "merge-base",
            "--is-ancestor",
            &commit_id,
            &request.publication_branch,
        ])
        .is_err()
    {
        return Err(inspect_error(format!(
            "commit {commit_id} is not on publication branch {}",
            request.publication_branch
        )));
    }

    if tree_dir.exists() {
        fs::remove_dir_all(&tree_dir)
            .map_err(|error| OrbitError::from_write_io(&tree_dir, error))?;
    }
    create_private_dir_all(&tree_dir)
        .map_err(|error| OrbitError::from_write_io(&tree_dir, error))?;
    git(&[
        "--git-dir",
        git_dir_s,
        "--work-tree",
        tree_dir_s,
        "checkout",
        "--force",
        "--detach",
        &commit_id,
    ])?;

    let git_parent = GitRunner::new(INSPECT_LABEL).single_parent(git_dir_s, &commit_id)?;

    let freshness = if commit_id == branch_tip {
        PublicationFreshness::Current
    } else {
        PublicationFreshness::Stale
    };
    Ok(FetchedSnapshot {
        tree_dir,
        branch: request.publication_branch.clone(),
        commit_id,
        git_parent,
        freshness,
    })
}

fn read_envelope(tree_dir: &Path) -> Result<PublicationEnvelope, OrbitError> {
    let path = tree_dir.join(PUBLICATION_ENVELOPE_FILE_NAME);
    let raw = fs::read_to_string(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            inspect_error("publication snapshot is missing orbit-task-publication.yaml")
        } else {
            OrbitError::from_write_io(&path, error)
        }
    })?;
    PublicationEnvelope::from_yaml(&raw)
}

fn assert_pairing(
    request: &ValidatedRequest,
    envelope: &PublicationEnvelope,
    fetched_branch: &str,
) -> Result<(), OrbitError> {
    field_mismatch(
        INSPECT_LABEL,
        "workspace",
        &request.workspace_id,
        &envelope.workspace_id,
    )?;
    field_mismatch(
        INSPECT_LABEL,
        "source repository fingerprint",
        &request.source_repository_fingerprint,
        &envelope.source_repository_fingerprint,
    )?;
    field_mismatch(
        INSPECT_LABEL,
        "publication id",
        &request.publication_id,
        &envelope.publication_id,
    )?;
    field_mismatch(
        INSPECT_LABEL,
        "authority",
        &request.authority_machine_id,
        &envelope.authority_machine_id,
    )?;
    field_mismatch(
        INSPECT_LABEL,
        "branch",
        &request.publication_branch,
        fetched_branch,
    )
}

fn read_validated_tasks(
    tree_dir: &Path,
    envelope: &PublicationEnvelope,
) -> Result<Vec<ValidatedPublicationBundle>, OrbitError> {
    let tasks_root = tree_dir.join(PUBLICATION_TASKS_DIR_NAME);
    let observed = published_task_ids(&tasks_root)?;
    if observed != envelope.task_ids {
        return Err(inspect_error(
            "publication task list does not match the tasks/ tree",
        ));
    }
    let mut bundles = Vec::with_capacity(envelope.task_ids.len());
    for task_id in &envelope.task_ids {
        let bundle_dir = tasks_root.join(task_id);
        restore_empty_artifact_dir(&bundle_dir)?;
        validate_jsonl_files(task_id, &bundle_dir)?;
        if envelope.attachment_policy == AttachmentPolicyKind::Omit
            && artifact_tree_has_files(&bundle_dir.join(TASK_ARTIFACTS_DIR_NAME))?
        {
            return Err(inspect_error(format!(
                "omit projection for task '{task_id}' still contains artifacts"
            )));
        }
        let bundle = read_bundle_at(&bundle_dir).map_err(|error| {
            inspect_error(format!(
                "task '{task_id}' failed publication bundle validation: {error}"
            ))
        })?;
        if bundle.envelope.id != *task_id {
            return Err(inspect_error(format!(
                "task '{task_id}' envelope id is '{}'",
                bundle.envelope.id
            )));
        }
        bundles.push(ValidatedPublicationBundle {
            bundle,
            source_dir: bundle_dir,
        });
    }
    Ok(bundles)
}

fn restore_empty_artifact_dir(bundle_dir: &Path) -> Result<(), OrbitError> {
    // Git does not store empty directories; recreate the canonical artifacts
    // layout in the disposable inspect tree when the snapshot had no blobs.
    let artifacts = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME);
    if !artifacts.exists() {
        let files = artifacts.join(TASK_ARTIFACT_FILES_DIR_NAME);
        create_private_dir_all(&files).map_err(|error| OrbitError::from_write_io(&files, error))?;
    }
    Ok(())
}

fn artifact_tree_has_files(path: &Path) -> Result<bool, OrbitError> {
    if !path.exists() {
        return Ok(false);
    }
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in
            fs::read_dir(&current).map_err(|error| OrbitError::from_write_io(&current, error))?
        {
            let entry = entry.map_err(|error| OrbitError::from_write_io(&current, error))?;
            let file_type = entry
                .file_type()
                .map_err(|error| OrbitError::from_write_io(&entry.path(), error))?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn published_task_ids(tasks_root: &Path) -> Result<Vec<String>, OrbitError> {
    if !tasks_root.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in
        fs::read_dir(tasks_root).map_err(|error| OrbitError::from_write_io(tasks_root, error))?
    {
        let entry = entry.map_err(|error| OrbitError::from_write_io(tasks_root, error))?;
        let name = entry.file_name();
        let Some(id) = name.to_str() else {
            return Err(inspect_error(
                "publication tasks/ contains a non-UTF-8 entry",
            ));
        };
        if id.starts_with('.') {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::from_write_io(&entry.path(), error))?;
        if !file_type.is_dir() {
            return Err(inspect_error(format!(
                "publication tasks/ contains unexpected file '{id}'"
            )));
        }
        ids.push(id.to_string());
    }
    ids.sort();
    Ok(ids)
}

fn assert_cache_origin(git_dir: &Path, expected_remote: &str) -> Result<(), OrbitError> {
    let observed = git(&[
        "--git-dir",
        path_str(git_dir)?,
        "remote",
        "get-url",
        "origin",
    ])?;
    if remotes_match(expected_remote, &observed) {
        return Ok(());
    }
    Err(inspect_error(format!(
        "consumer cache origin '{}' does not match requested remote '{}'",
        redact_git_remote(&observed),
        redact_git_remote(expected_remote)
    )))
}

fn git(args: &[&str]) -> Result<String, OrbitError> {
    GitRunner::new(INSPECT_LABEL).run(args)
}

fn path_str(path: &Path) -> Result<&str, OrbitError> {
    super::git::path_str(path, INSPECT_LABEL)
}

fn inspect_error(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(format!("{INSPECT_LABEL}: {}", message.into()))
}

fn identity_error(error: orbit_types::identity::IdentityError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}
