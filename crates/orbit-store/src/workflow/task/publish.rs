//! Owner-only Git transport for task-publication snapshots.
//!
//! Publication is explicit derived work: it never participates in canonical
//! task-write acknowledgement, and it never becomes a second task authority.
//! The transport therefore only ever advances one dedicated publication branch
//! from the declared workspace owner, as a fast-forward compare-and-swap
//! against the branch tip it actually observed. It never merges, rebases,
//! replays task operations, force-pushes, selects by timestamp, or creates an
//! alternate conflict branch, and it never touches the source repository, its
//! worktree, refs, or configured remotes: all Git work happens in an
//! Orbit-owned private cache under the caller-supplied `cache_dir`.
//!
//! Ownership facts (declared owner, local checkout role, binding) live in the
//! machine-local workspace registry owned by `orbit-registry`. They are inputs
//! here rather than lookups, so this crate keeps its narrow dependency set; the
//! caller passes what it read, and every field is re-verified against the
//! coordination registry and the remote envelope before anything is built.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::identity::{validate_machine_id, validate_registry_identifier};
use orbit_types::workspace::{
    canonicalize_publication_branch, redact_git_remote, validate_git_commit_id,
    validate_last_success, validate_source_repository_fingerprint,
};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::fs::yaml::write_yaml_atomic_with;

use super::git::{
    GitRunner, field_mismatch, path_str, remote_has_password, remotes_match, short_branch,
};
use super::publication::{
    AttachmentPolicy, AttachmentSensitivityScanner, PUBLICATION_ENVELOPE_FILE_NAME,
    PUBLICATION_TASKS_DIR_NAME, PublicationEnvelope, PublicationSnapshotMetadata,
    assert_envelope_parent_lineage, build_publication_snapshot_from_task_workspace,
};

/// Error prefix and command label for every transport failure.
const PUBLISH_LABEL: &str = "task publication";
/// Private record of a push that was issued but not yet confirmed as recorded.
const PENDING_FILE_NAME: &str = "pending-publication.yaml";
const PENDING_FORMAT_VERSION: u32 = 1;

/// Local checkout role of the caller. Only the declared owner may publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationCallerRole {
    Owner,
    Replica,
}

/// Owner-local record of the last publication that was pushed *and* persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationLastSuccess {
    pub generation: u64,
    pub commit: String,
}

/// Binding facts and commit metadata for one publication attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationPublishRequest {
    /// Logical workspace identity recorded in the publication envelope.
    pub workspace_id: String,
    /// Existing task-registry partition to enumerate for this snapshot.
    pub task_workspace_id: String,
    pub source_repository_fingerprint: String,
    pub publication_id: String,
    pub authority_machine_id: String,
    /// Machine running this attempt; must be the declared authority.
    pub local_machine_id: String,
    pub caller_role: PublicationCallerRole,
    pub publication_remote: String,
    pub publication_branch: String,
    /// Orbit-owned private cache root; must be outside the source checkout.
    pub cache_dir: PathBuf,
    pub published_at: DateTime<Utc>,
    pub last_success: Option<PublicationLastSuccess>,
}

/// What a publication attempt did to the publication branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationPublishStatus {
    /// Created the configured branch in a previously empty repository.
    Initialized,
    /// Fast-forwarded the branch with a new generation.
    Advanced,
    /// The branch tip already carries this exact projection; nothing pushed.
    Unchanged,
    /// A previous push landed but was never recorded locally; nothing pushed.
    Reconciled,
}

/// Result the owner records as its new last-success state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationPublishOutcome {
    pub status: PublicationPublishStatus,
    pub branch: String,
    pub commit_id: String,
    pub generation: u64,
    pub previous_publication: Option<String>,
    /// Branch tip observed before the compare-and-swap, if the branch existed.
    pub observed_tip: Option<String>,
    pub included_attachment_bytes: u64,
    pub omitted_attachment_bytes: u64,
}

/// Publish this workspace's validated task snapshot to its dedicated
/// publication repository.
///
/// Fails closed — leaving the remote branch and the caller's last-success
/// record untouched — when the caller is not the declared owner, the registered
/// workspace or source fingerprint disagrees, the repository is non-empty
/// without a matching publication envelope, the branch moved, or any bundle
/// fails validation.
pub fn publish_task_snapshot(
    registry: &TaskRegistryStore,
    request: PublicationPublishRequest,
    policy: &AttachmentPolicy,
    scanner: Option<&dyn AttachmentSensitivityScanner>,
) -> Result<PublicationPublishOutcome, OrbitError> {
    let request = validate_request(request)?;
    assert_registered_workspace(registry, &request)?;
    let cache = PublicationCache::open(registry, &request)?;
    let observed = cache.observe_remote(&request)?;
    let action = decide_action(&cache, &request, &observed)?;
    let (generation, previous) = match action {
        PublishAction::Reconcile(outcome) => return Ok(*outcome),
        PublishAction::Initialize => (1, None),
        PublishAction::Advance {
            generation,
            previous,
        } => (generation, Some(previous)),
    };

    let staged = cache.stage_snapshot(
        registry,
        &request,
        generation,
        previous.clone(),
        policy,
        scanner,
    )?;
    let observed_tip = observed.tip.as_ref().map(|tip| tip.commit.clone());
    if let Some(tip) = &observed.tip
        && tip.tasks_tree == staged.tasks_tree
        && same_publication_content(&tip.envelope, &staged.envelope)
    {
        return Ok(PublicationPublishOutcome {
            status: PublicationPublishStatus::Unchanged,
            branch: request.publication_branch.clone(),
            commit_id: tip.commit.clone(),
            generation: tip.envelope.generation,
            previous_publication: tip.envelope.previous_publication.clone(),
            observed_tip,
            included_attachment_bytes: staged.included_attachment_bytes,
            omitted_attachment_bytes: staged.omitted_attachment_bytes,
        });
    }

    let commit = cache.commit_snapshot(&request, &staged, generation, previous.as_deref())?;
    cache.write_pending(&PendingPublication {
        format_version: PENDING_FORMAT_VERSION,
        publication_id: request.publication_id.clone(),
        workspace_id: request.workspace_id.clone(),
        publication_branch: request.publication_branch.clone(),
        generation,
        commit: commit.clone(),
        previous_publication: previous.clone(),
    })?;
    cache.push_fast_forward(&request, &commit, observed_tip.as_deref())?;

    Ok(PublicationPublishOutcome {
        status: if previous.is_none() {
            PublicationPublishStatus::Initialized
        } else {
            PublicationPublishStatus::Advanced
        },
        branch: request.publication_branch.clone(),
        commit_id: commit,
        generation,
        previous_publication: previous,
        observed_tip,
        included_attachment_bytes: staged.included_attachment_bytes,
        omitted_attachment_bytes: staged.omitted_attachment_bytes,
    })
}

struct ValidatedRequest {
    workspace_id: String,
    task_workspace_id: String,
    source_repository_fingerprint: String,
    publication_id: String,
    authority_machine_id: String,
    publication_remote: String,
    publication_branch: String,
    cache_dir: PathBuf,
    published_at: DateTime<Utc>,
    last_success: Option<PublicationLastSuccess>,
}

fn validate_request(request: PublicationPublishRequest) -> Result<ValidatedRequest, OrbitError> {
    if request.caller_role != PublicationCallerRole::Owner {
        return Err(OrbitError::PolicyDenied(format!(
            "{PUBLISH_LABEL}: workspace '{}' is a replica checkout; only the declared owner may publish",
            request.workspace_id
        )));
    }
    validate_registry_identifier("workspace_id", &request.workspace_id).map_err(identity_error)?;
    validate_registry_identifier("task_workspace_id", &request.task_workspace_id)
        .map_err(identity_error)?;
    validate_registry_identifier("publication_id", &request.publication_id)
        .map_err(identity_error)?;
    validate_machine_id(&request.authority_machine_id).map_err(identity_error)?;
    validate_machine_id(&request.local_machine_id).map_err(identity_error)?;
    if request.local_machine_id != request.authority_machine_id {
        return Err(OrbitError::PolicyDenied(format!(
            "{PUBLISH_LABEL}: workspace '{}' is owned by '{}', not local machine '{}'",
            request.workspace_id, request.authority_machine_id, request.local_machine_id
        )));
    }
    validate_source_repository_fingerprint(&request.source_repository_fingerprint)
        .map_err(workspace_error)?;
    if request.publication_remote.trim().is_empty() {
        return Err(publish_error("publication remote must not be empty"));
    }
    if remote_has_password(&request.publication_remote) {
        return Err(publish_error(format!(
            "publication remote '{}' must not contain credentials",
            redact_git_remote(&request.publication_remote)
        )));
    }
    let publication_branch =
        canonicalize_publication_branch(&request.publication_branch).map_err(workspace_error)?;
    if request.cache_dir.as_os_str().is_empty() {
        return Err(publish_error("publication cache directory is required"));
    }
    let last_success = match request.last_success {
        Some(last) => {
            validate_last_success(Some(last.generation), Some(&last.commit))
                .map_err(workspace_error)?;
            Some(PublicationLastSuccess {
                generation: last.generation,
                commit: last.commit.to_ascii_lowercase(),
            })
        }
        None => None,
    };
    Ok(ValidatedRequest {
        workspace_id: request.workspace_id,
        task_workspace_id: request.task_workspace_id,
        source_repository_fingerprint: request.source_repository_fingerprint,
        publication_id: request.publication_id,
        authority_machine_id: request.authority_machine_id,
        publication_remote: request.publication_remote,
        publication_branch,
        cache_dir: request.cache_dir,
        published_at: request.published_at,
        last_success,
    })
}

/// The coordination registry — not the caller — decides which workspace and
/// source repository this publication belongs to.
fn assert_registered_workspace(
    registry: &TaskRegistryStore,
    request: &ValidatedRequest,
) -> Result<(), OrbitError> {
    let binding = registry
        .find_workspace_binding(&request.task_workspace_id)?
        .ok_or_else(|| {
            publish_error(format!(
                "task workspace '{}' is not registered in the coordination registry",
                request.task_workspace_id
            ))
        })?;
    if binding.partition_id != request.task_workspace_id {
        return Err(publish_error(format!(
            "task workspace selector '{}' resolved to unexpected workspace '{}'",
            request.task_workspace_id, binding.partition_id
        )));
    }
    match binding.repo_fingerprint.as_deref() {
        Some(fingerprint) if fingerprint == request.source_repository_fingerprint => Ok(()),
        Some(_) => Err(publish_error(format!(
            "task workspace '{}' publication fingerprint does not match its registered source remote",
            request.task_workspace_id
        ))),
        None => Err(publish_error(format!(
            "task workspace '{}' has no registered source-repository identity",
            request.task_workspace_id
        ))),
    }
}

/// Orbit-owned private clone plus object cache for one publication lineage.
struct PublicationCache {
    root: PathBuf,
    git_dir: PathBuf,
    pending_path: PathBuf,
}

impl PublicationCache {
    fn open(registry: &TaskRegistryStore, request: &ValidatedRequest) -> Result<Self, OrbitError> {
        assert_outside_source_checkout(registry, request)?;
        let root = request
            .cache_dir
            .join(&request.publication_id)
            .join("publish");
        fs::create_dir_all(&root).map_err(|error| OrbitError::from_write_io(&root, error))?;
        let cache = Self {
            git_dir: root.join("origin.git"),
            pending_path: root.join(PENDING_FILE_NAME),
            root,
        };
        if cache.git_dir.join("HEAD").is_file() {
            cache.assert_cache_origin(&request.publication_remote)?;
        } else {
            if cache.git_dir.exists() {
                fs::remove_dir_all(&cache.git_dir)
                    .map_err(|error| OrbitError::from_write_io(&cache.git_dir, error))?;
            }
            let git_dir = cache.git_dir_str()?;
            runner().run(&[
                "init",
                "--bare",
                "--quiet",
                "-b",
                short_branch(&request.publication_branch),
                git_dir,
            ])?;
            runner().run(&[
                "--git-dir",
                git_dir,
                "remote",
                "add",
                "origin",
                &request.publication_remote,
            ])?;
        }
        Ok(cache)
    }

    fn git_dir_str(&self) -> Result<&str, OrbitError> {
        path_str(&self.git_dir, PUBLISH_LABEL)
    }

    fn assert_cache_origin(&self, expected_remote: &str) -> Result<(), OrbitError> {
        let observed = runner().run(&[
            "--git-dir",
            self.git_dir_str()?,
            "remote",
            "get-url",
            "origin",
        ])?;
        if remotes_match(expected_remote, &observed) {
            return Ok(());
        }
        Err(publish_error(format!(
            "publication cache origin '{}' does not match the bound remote '{}'",
            redact_git_remote(&observed),
            redact_git_remote(expected_remote)
        )))
    }

    /// Fetch the configured branch and validate the tip's envelope, pairing,
    /// and lineage before any snapshot is built.
    fn observe_remote(&self, request: &ValidatedRequest) -> Result<RemoteState, OrbitError> {
        let git_dir = self.git_dir_str()?;
        let listing = runner().run(&["--git-dir", git_dir, "ls-remote", "origin"])?;
        let branch_present = listing
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .any(|(_, name)| name.trim() == request.publication_branch);
        if !branch_present {
            if listing.trim().is_empty() {
                return Ok(RemoteState { tip: None });
            }
            return Err(publish_error(format!(
                "publication repository is not empty and does not carry branch '{}'; refusing to initialize, adopt, or overwrite it",
                request.publication_branch
            )));
        }

        let refspec = format!(
            "{}:{}",
            request.publication_branch, request.publication_branch
        );
        runner().run(&["--git-dir", git_dir, "fetch", "--force", "origin", &refspec])?;
        let commit = runner()
            .run(&[
                "--git-dir",
                git_dir,
                "rev-parse",
                &request.publication_branch,
            ])?
            .to_ascii_lowercase();
        let envelope = self.read_envelope(&commit)?;
        let parent = runner().single_parent(self.git_dir_str()?, &commit)?;
        assert_pairing(request, &envelope)?;
        assert_envelope_parent_lineage(&envelope, parent.as_deref(), PUBLISH_LABEL)?;
        Ok(RemoteState {
            tip: Some(RemoteTip {
                tasks_tree: self.read_tasks_tree(&commit)?,
                commit,
                envelope,
            }),
        })
    }

    fn read_envelope(&self, commit: &str) -> Result<PublicationEnvelope, OrbitError> {
        let spec = format!("{commit}:{PUBLICATION_ENVELOPE_FILE_NAME}");
        let attempt = runner().try_run(&["--git-dir", self.git_dir_str()?, "show", &spec])?;
        if !attempt.success {
            return Err(publish_error(format!(
                "publication branch tip {commit} does not carry {PUBLICATION_ENVELOPE_FILE_NAME}; refusing to adopt or overwrite it"
            )));
        }
        PublicationEnvelope::from_yaml(&attempt.stdout)
    }

    /// Object id of the `tasks/` tree, or `None` when the snapshot has no tasks.
    fn read_tasks_tree(&self, tree_ish: &str) -> Result<Option<String>, OrbitError> {
        let spec = format!("{tree_ish}:{PUBLICATION_TASKS_DIR_NAME}");
        let attempt = runner().try_run(&[
            "--git-dir",
            self.git_dir_str()?,
            "rev-parse",
            "--verify",
            "--quiet",
            &spec,
        ])?;
        Ok(attempt
            .success
            .then(|| attempt.stdout.trim().to_ascii_lowercase())
            .filter(|oid| !oid.is_empty()))
    }

    /// Build the snapshot tree in a disposable directory inside the private
    /// cache and record it in a throwaway index. Nothing here can touch the
    /// source checkout.
    fn stage_snapshot(
        &self,
        registry: &TaskRegistryStore,
        request: &ValidatedRequest,
        generation: u64,
        previous_publication: Option<String>,
        policy: &AttachmentPolicy,
        scanner: Option<&dyn AttachmentSensitivityScanner>,
    ) -> Result<StagedSnapshot, OrbitError> {
        let temp = tempfile::Builder::new()
            .prefix(".orbit-publish-")
            .tempdir_in(&self.root)
            .map_err(|error| OrbitError::from_write_io(&self.root, error))?;
        let tree = temp.path().join("tree");
        let outcome = build_publication_snapshot_from_task_workspace(
            registry,
            &tree,
            PublicationSnapshotMetadata {
                publication_id: request.publication_id.clone(),
                workspace_id: request.workspace_id.clone(),
                source_repository_fingerprint: request.source_repository_fingerprint.clone(),
                authority_machine_id: request.authority_machine_id.clone(),
                generation,
                published_at: request.published_at,
                previous_publication,
            },
            &request.task_workspace_id,
            policy,
            scanner,
        )?;

        let index = temp.path().join("index");
        let indexed = runner().with_env(vec![(
            "GIT_INDEX_FILE",
            path_str(&index, PUBLISH_LABEL)?.to_string(),
        )]);
        let git_dir = self.git_dir_str()?;
        let work_tree = path_str(&tree, PUBLISH_LABEL)?;
        indexed.run(&[
            "-C",
            work_tree,
            "--git-dir",
            git_dir,
            "--work-tree",
            work_tree,
            "add",
            "--all",
            "--force",
        ])?;
        let tree_oid = indexed
            .run(&["--git-dir", git_dir, "write-tree"])?
            .to_ascii_lowercase();
        Ok(StagedSnapshot {
            tasks_tree: self.read_tasks_tree(&tree_oid)?,
            tree_oid,
            envelope: outcome.envelope,
            included_attachment_bytes: outcome.included_attachment_bytes,
            omitted_attachment_bytes: outcome.omitted_attachment_bytes,
            _temp: temp,
        })
    }

    /// Commit the staged tree with a deterministic identity and timestamp, so
    /// an identical snapshot at the same parent always yields the same commit.
    fn commit_snapshot(
        &self,
        request: &ValidatedRequest,
        staged: &StagedSnapshot,
        generation: u64,
        parent: Option<&str>,
    ) -> Result<String, OrbitError> {
        let stamp = request
            .published_at
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let identity = format!("{}@orbit.invalid", request.authority_machine_id);
        let commit_runner = runner().with_env(vec![
            ("GIT_AUTHOR_NAME", request.authority_machine_id.clone()),
            ("GIT_AUTHOR_EMAIL", identity.clone()),
            ("GIT_AUTHOR_DATE", stamp.clone()),
            ("GIT_COMMITTER_NAME", request.authority_machine_id.clone()),
            ("GIT_COMMITTER_EMAIL", identity),
            ("GIT_COMMITTER_DATE", stamp),
        ]);
        let message = format!(
            "publication {} generation {generation}",
            request.publication_id
        );
        let mut args = vec![
            "--git-dir",
            self.git_dir_str()?,
            "commit-tree",
            &staged.tree_oid,
        ];
        if let Some(parent) = parent {
            args.extend_from_slice(&["-p", parent]);
        }
        args.extend_from_slice(&["-m", &message]);
        Ok(commit_runner.run(&args)?.to_ascii_lowercase())
    }

    /// Push as an exact compare-and-swap of the observed tip. Ordinary Git
    /// fast-forward is not enough: a branch deleted or rewound after observation
    /// can still accept a descendant and recreate or advance the ref. The lease
    /// names the expected old object (or requires the ref to stay absent); the
    /// refspec is not force-prefixed, so a matching lease cannot replace
    /// non-fast-forward history. A moved branch is an authority conflict, never
    /// something to merge or force.
    fn push_fast_forward(
        &self,
        request: &ValidatedRequest,
        commit: &str,
        observed_tip: Option<&str>,
    ) -> Result<(), OrbitError> {
        #[cfg(test)]
        run_before_push_hook();

        let git_dir = self.git_dir_str()?;
        let lease = compare_and_swap_lease(&request.publication_branch, observed_tip);
        let refspec = format!("{commit}:{}", request.publication_branch);
        let attempt = runner().try_run(&[
            "--git-dir",
            git_dir,
            "push",
            "--atomic",
            &lease,
            "origin",
            &refspec,
        ])?;
        if !attempt.success {
            let current = self.remote_branch_tip(request)?;
            if current.as_deref() != observed_tip {
                return Err(publish_error(format!(
                    "publication branch '{}' moved during publication: observed {}, remote is now {}; resolve the publication authority before publishing again",
                    request.publication_branch,
                    observed_tip.unwrap_or("empty"),
                    current.as_deref().unwrap_or("empty")
                )));
            }
            return Err(publish_error(format!(
                "publication push to '{}' failed: {}",
                redact_git_remote(&request.publication_remote),
                redact_remote(&attempt.stderr, &request.publication_remote)
            )));
        }
        match self.remote_branch_tip(request)? {
            Some(tip) if tip == commit => Ok(()),
            other => Err(publish_error(format!(
                "publication branch '{}' is {} after pushing {commit}",
                request.publication_branch,
                other.unwrap_or_else(|| "empty".to_string())
            ))),
        }
    }

    fn remote_branch_tip(&self, request: &ValidatedRequest) -> Result<Option<String>, OrbitError> {
        let listing = runner().run(&[
            "--git-dir",
            self.git_dir_str()?,
            "ls-remote",
            "origin",
            &request.publication_branch,
        ])?;
        Ok(listing
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .find(|(_, name)| name.trim() == request.publication_branch)
            .map(|(oid, _)| oid.trim().to_ascii_lowercase()))
    }

    fn read_pending(
        &self,
        request: &ValidatedRequest,
    ) -> Result<Option<PendingPublication>, OrbitError> {
        let raw = match fs::read_to_string(&self.pending_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(OrbitError::from_write_io(&self.pending_path, error)),
        };
        // A private cache artifact: anything unreadable or belonging to another
        // binding is discarded, never trusted and never fatal.
        let pending: Option<PendingPublication> = serde_yaml::from_str(&raw).ok();
        let pending = pending.filter(|pending| {
            pending.format_version == PENDING_FORMAT_VERSION
                && pending.publication_id == request.publication_id
                && pending.workspace_id == request.workspace_id
                && pending.publication_branch == request.publication_branch
                && validate_git_commit_id(&pending.commit).is_ok()
        });
        if pending.is_none() {
            self.remove_pending()?;
        }
        Ok(pending)
    }

    fn write_pending(&self, pending: &PendingPublication) -> Result<(), OrbitError> {
        write_yaml_atomic_with(&self.pending_path, pending, |error| {
            OrbitError::Store(format!(
                "failed to encode pending publication record: {error}"
            ))
        })
    }

    fn remove_pending(&self) -> Result<(), OrbitError> {
        match fs::remove_file(&self.pending_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OrbitError::from_write_io(&self.pending_path, error)),
        }
    }
}

struct RemoteState {
    tip: Option<RemoteTip>,
}

struct RemoteTip {
    commit: String,
    envelope: PublicationEnvelope,
    tasks_tree: Option<String>,
}

struct StagedSnapshot {
    tree_oid: String,
    tasks_tree: Option<String>,
    envelope: PublicationEnvelope,
    included_attachment_bytes: u64,
    omitted_attachment_bytes: u64,
    /// Dropped last: removes the staged tree and throwaway index.
    _temp: TempDir,
}

enum PublishAction {
    Initialize,
    Advance { generation: u64, previous: String },
    Reconcile(Box<PublicationPublishOutcome>),
}

/// Reconcile the observed branch tip with the owner-local last-success record
/// and any pending push, or refuse the run.
fn decide_action(
    cache: &PublicationCache,
    request: &ValidatedRequest,
    observed: &RemoteState,
) -> Result<PublishAction, OrbitError> {
    let pending = cache.read_pending(request)?;
    let Some(tip) = &observed.tip else {
        if let Some(last) = &request.last_success {
            return Err(publish_error(format!(
                "publication repository is empty but generation {} at commit {} was recorded locally; resolve the publication binding before publishing again",
                last.generation, last.commit
            )));
        }
        // A pending push that never landed: private state, cleaned up here.
        cache.remove_pending()?;
        return Ok(PublishAction::Initialize);
    };

    let pending_landed = pending
        .as_ref()
        .is_some_and(|pending| pending.commit == tip.commit);
    let recorded = request
        .last_success
        .as_ref()
        .is_some_and(|last| last.commit == tip.commit);
    if pending_landed && !recorded {
        cache.remove_pending()?;
        return Ok(PublishAction::Reconcile(Box::new(
            PublicationPublishOutcome {
                status: PublicationPublishStatus::Reconciled,
                branch: request.publication_branch.clone(),
                commit_id: tip.commit.clone(),
                generation: tip.envelope.generation,
                previous_publication: tip.envelope.previous_publication.clone(),
                observed_tip: Some(tip.commit.clone()),
                included_attachment_bytes: 0,
                omitted_attachment_bytes: 0,
            },
        )));
    }

    match &request.last_success {
        Some(last) if recorded => {
            if last.generation != tip.envelope.generation {
                return Err(publish_error(format!(
                    "publication commit {} records generation {} but the owner recorded generation {}",
                    tip.commit, tip.envelope.generation, last.generation
                )));
            }
            cache.remove_pending()?;
        }
        Some(last) => {
            return Err(publish_error(format!(
                "publication branch '{}' is at {} but the owner last published {} at generation {}; resolve the publication authority before publishing again",
                request.publication_branch, tip.commit, last.commit, last.generation
            )));
        }
        // No local record: the matching envelope is the only authority evidence,
        // so an unmatched pending push means someone else moved the branch.
        None if pending.is_some() => {
            return Err(publish_error(format!(
                "publication branch '{}' is at {}, which is not the commit this owner pushed; resolve the publication authority before publishing again",
                request.publication_branch, tip.commit
            )));
        }
        None => {}
    }

    let generation = tip.envelope.generation.checked_add(1).ok_or_else(|| {
        publish_error("publication generation would overflow; rebind the publication lineage")
    })?;
    Ok(PublishAction::Advance {
        generation,
        previous: tip.commit.clone(),
    })
}

fn assert_pairing(
    request: &ValidatedRequest,
    envelope: &PublicationEnvelope,
) -> Result<(), OrbitError> {
    field_mismatch(
        PUBLISH_LABEL,
        "workspace",
        &request.workspace_id,
        &envelope.workspace_id,
    )?;
    field_mismatch(
        PUBLISH_LABEL,
        "source repository fingerprint",
        &request.source_repository_fingerprint,
        &envelope.source_repository_fingerprint,
    )?;
    field_mismatch(
        PUBLISH_LABEL,
        "publication id",
        &request.publication_id,
        &envelope.publication_id,
    )?;
    field_mismatch(
        PUBLISH_LABEL,
        "authority",
        &request.authority_machine_id,
        &envelope.authority_machine_id,
    )
}

/// True when two envelopes describe the same projection. `generation`,
/// `published_at`, and `previous_publication` are lineage bookkeeping, not
/// content, so a re-run that changed nothing must not create a commit.
fn same_publication_content(left: &PublicationEnvelope, right: &PublicationEnvelope) -> bool {
    left.format_version == right.format_version
        && left.publication_id == right.publication_id
        && left.workspace_id == right.workspace_id
        && left.source_repository_fingerprint == right.source_repository_fingerprint
        && left.authority_machine_id == right.authority_machine_id
        && left.task_schema_version == right.task_schema_version
        && left.attachment_policy == right.attachment_policy
        && left.task_ids == right.task_ids
        && left.omitted_attachments == right.omitted_attachments
}

/// The publication cache must never live inside the source repository.
fn assert_outside_source_checkout(
    registry: &TaskRegistryStore,
    request: &ValidatedRequest,
) -> Result<(), OrbitError> {
    let Some(checkout) = registry.find_workspace_checkout(&request.task_workspace_id)? else {
        return Ok(());
    };
    let cache = std::path::absolute(&request.cache_dir)
        .map_err(|error| OrbitError::from_write_io(&request.cache_dir, error))?;
    for source in [&checkout.repo_root, &checkout.workspace_path] {
        let source = std::path::absolute(source)
            .map_err(|error| OrbitError::from_write_io(source, error))?;
        if cache.starts_with(&source) {
            return Err(publish_error(
                "publication cache directory must live outside the source repository and its worktree",
            ));
        }
    }
    Ok(())
}

/// Private record of a push that was issued but not yet confirmed as recorded
/// by the owner. It lets the next run reconcile by commit id instead of
/// publishing a duplicate or divergent generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPublication {
    format_version: u32,
    publication_id: String,
    workspace_id: String,
    publication_branch: String,
    generation: u64,
    commit: String,
    previous_publication: Option<String>,
}

/// Branch-scoped expected-old-object lease. An empty expect (`<branch>:`)
/// requires the ref to still be absent; a commit id requires that exact tip.
fn compare_and_swap_lease(branch: &str, observed_tip: Option<&str>) -> String {
    match observed_tip {
        Some(oid) => format!("--force-with-lease={branch}:{oid}"),
        None => format!("--force-with-lease={branch}:"),
    }
}

fn runner() -> GitRunner<'static> {
    GitRunner::new(PUBLISH_LABEL)
}

#[cfg(test)]
thread_local! {
    static BEFORE_PUSH: std::cell::RefCell<Option<Box<dyn FnOnce() + 'static>>> =
        std::cell::RefCell::new(None);
}

/// Install a one-shot callback that runs after the pending record is written
/// and immediately before the compare-and-swap push. Tests use this to mutate
/// the remote between observation and push.
#[cfg(test)]
pub(crate) fn set_before_push_hook(hook: impl FnOnce() + 'static) {
    BEFORE_PUSH.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn clear_before_push_hook() {
    BEFORE_PUSH.with(|cell| cell.borrow_mut().take());
}

#[cfg(test)]
fn run_before_push_hook() {
    if let Some(hook) = BEFORE_PUSH.with(|cell| cell.borrow_mut().take()) {
        hook();
    }
}

fn redact_remote(message: &str, remote: &str) -> String {
    message.trim().replace(remote, &redact_git_remote(remote))
}

fn publish_error(message: impl Into<String>) -> OrbitError {
    OrbitError::InvalidInput(format!("{PUBLISH_LABEL}: {}", message.into()))
}

fn workspace_error(error: orbit_types::workspace::WorkspaceError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

fn identity_error(error: orbit_types::identity::IdentityError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}
