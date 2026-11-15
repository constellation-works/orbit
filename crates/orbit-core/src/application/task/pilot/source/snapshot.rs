//! Landing-branch source snapshot for task-pilot prepare/apply.

use std::path::Path;

use orbit_common::fs::git::{GIT_REMOTE_TIMEOUT, run_git, try_with_git_fetch_lock};
use orbit_engine::DispatchError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::git::{
    FetchLockError, fetch_origin_branch, git, git_tree_path, has_origin_remote, is_commit_id,
    is_git_work_tree, publish_remote_tracking_ref, rev_parse_commit, tree_of,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceSnapshot {
    pub base_branch: String,
    pub source_ref: String,
    pub source_revision: String,
    // Retained in prepared checkpoints for compatibility with earlier runs.
    // New preparations never move the primary and always emit false.
    pub fast_forwarded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitPathKind {
    Blob,
    Tree,
    Missing,
    Other,
}

impl SourceSnapshot {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "base_branch": self.base_branch,
            "source_ref": self.source_ref,
            "source_revision": self.source_revision,
            "fast_forwarded": self.fast_forwarded,
        })
    }

    pub(crate) fn from_prepared(
        prepared: &Value,
        action: &str,
    ) -> Result<Option<Self>, DispatchError> {
        let Some(source) = prepared.get("source") else {
            return Ok(None);
        };
        if source.is_null() {
            return Ok(None);
        }
        let object = source.as_object().ok_or_else(|| {
            DispatchError::action_failed(action, "prepared.source must be an object or null")
        })?;
        let source_revision = object
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(source_revision) = source_revision else {
            return Ok(None);
        };
        if !is_commit_id(source_revision) {
            return Err(DispatchError::action_failed(
                action,
                format!(
                    "prepared.source.source_revision {source_revision:?} is not a full commit id"
                ),
            ));
        }
        let base_branch = object
            .get("base_branch")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    "prepared.source.base_branch must be a non-empty string",
                )
            })?;
        let source_ref = object
            .get("source_ref")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    "prepared.source.source_ref must be a non-empty string",
                )
            })?;
        Ok(Some(Self {
            base_branch: base_branch.to_string(),
            source_ref: source_ref.to_string(),
            source_revision: source_revision.to_string(),
            fast_forwarded: object
                .get("fast_forwarded")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }))
    }

    /// The tree id of the pinned revision. An inspection result proves it
    /// read the pinned source by reporting this value as `observed_tree`
    /// (`git rev-parse HEAD^{tree}` in its checkout): the envelope carries
    /// the commit id, which a blind result could echo back, but never the
    /// tree id, which only reading the commit object yields. Never include
    /// the expected tree id in agent-visible output (a repair attempt reads
    /// the validation errors and could echo it).
    pub(crate) fn tree_id(&self, action: &str, workspace: &Path) -> Result<String, DispatchError> {
        tree_of(workspace, &self.source_revision)
            .map_err(|error| {
                DispatchError::action_failed(
                    action,
                    format!("git rev-parse failed in '{}': {error}", workspace.display()),
                )
            })?
            .map_err(|stderr| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "prepared source revision {} ({}) has no readable tree in this \
                         repository: {stderr}",
                        self.source_revision, self.source_ref,
                    ),
                )
            })
    }

    pub(crate) fn ensure_commit(
        &self,
        action: &str,
        workspace: &Path,
    ) -> Result<(), DispatchError> {
        let spec = format!("{}^{{commit}}", self.source_revision);
        let output = git(action, workspace, &["rev-parse", "--verify", &spec])?;
        if output.success {
            Ok(())
        } else {
            Err(DispatchError::action_failed(
                action,
                format!(
                    "prepared source revision {} ({}) is not a commit in this repository: {}",
                    self.source_revision,
                    self.source_ref,
                    output.stderr.trim()
                ),
            ))
        }
    }

    /// When the pinned revision was committed and how old it was at
    /// preparation, or `null` when Git cannot say [ORB-14476]. State pilots
    /// best-effort fetch origin before pinning; this also exposes the age of
    /// the local fallback when that fetch is unavailable.
    pub(in crate::application::task::pilot) fn age(&self, workspace: &Path) -> Value {
        // `committer <name> <email> <epoch> <zone>` in the raw commit header.
        let committed = run_git(workspace, &["cat-file", "commit", &self.source_revision])
            .ok()
            .filter(|output| output.success)
            .and_then(|output| {
                let committer = output
                    .stdout
                    .lines()
                    .take_while(|line| !line.is_empty())
                    .find_map(|line| line.strip_prefix("committer "))?
                    .to_owned();
                let epoch = committer.rsplit(' ').nth(1)?.parse::<i64>().ok()?;
                chrono::DateTime::from_timestamp(epoch, 0)
            });
        match committed {
            Some(at) => json!({
                "committed_at": at,
                "age_seconds": (chrono::Utc::now() - at).num_seconds().max(0),
            }),
            None => Value::Null,
        }
    }

    /// Text of the blob at repository-relative `path` at the pinned revision.
    ///
    /// `None` when the path is not a safe repository-relative blob path or no
    /// blob is there. A `:` is rejected so the `revision:path` spec cannot
    /// be split into a different object. Oversized or binary blobs are an
    /// error: a cfg-gate check must not treat them as missing.
    pub(crate) fn file_text(
        &self,
        action: &str,
        workspace: &Path,
        path: &str,
    ) -> Result<Option<String>, DispatchError> {
        if git_tree_path(Path::new(path)).as_deref() != Some(path) || path.contains(':') {
            return Ok(None);
        }
        if self.path_kind(action, workspace, Path::new(path))? != GitPathKind::Blob {
            return Ok(None);
        }
        let output = git(
            action,
            workspace,
            &[
                "cat-file",
                "blob",
                &format!("{}:{path}", self.source_revision),
            ],
        )?;
        if !output.success {
            return Err(DispatchError::action_failed(
                action,
                format!("read {path} at the pinned source: {}", output.stderr.trim()),
            ));
        }
        const MAX_BYTES: usize = 2_000_000;
        if output.stdout.len() > MAX_BYTES || output.stdout.contains('\0') {
            return Err(DispatchError::action_failed(
                action,
                format!("read {path} at the pinned source: file is not a source text file"),
            ));
        }
        Ok(Some(output.stdout))
    }

    pub(crate) fn path_kind(
        &self,
        action: &str,
        workspace: &Path,
        anchor: &Path,
    ) -> Result<GitPathKind, DispatchError> {
        let output = if anchor == Path::new(".") {
            // The root is the pinned commit's tree object, not an entry in
            // that tree. Query it explicitly without consulting the checkout.
            git(
                action,
                workspace,
                &[
                    "cat-file",
                    "-t",
                    &format!("{}^{{tree}}", self.source_revision),
                ],
            )?
        } else {
            let relative = git_tree_path(anchor).ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!(
                        "selector anchor {} is not a repository-relative path",
                        anchor.display()
                    ),
                )
            })?;
            git(
                action,
                workspace,
                &[
                    "ls-tree",
                    "--format=%(objectmode) %(objecttype)",
                    &self.source_revision,
                    "--",
                    &format!(":(literal){relative}"),
                ],
            )?
        };
        if !output.success {
            return Err(DispatchError::action_failed(
                action,
                format!("read pinned source tree: {}", output.stderr.trim()),
            ));
        }
        Ok(match output.stdout.trim() {
            "100644 blob" | "100755 blob" => GitPathKind::Blob,
            "tree" | "040000 tree" => GitPathKind::Tree,
            "" => GitPathKind::Missing,
            _ => GitPathKind::Other,
        })
    }
}

/// The prepared-checkpoint key that marks a run admitted under the
/// observed-revision proof contract. Its results must carry the proof.
/// A checkpoint without it was prepared before the proof existed: its
/// results are checked only when they report one, so the upgrade keeps the
/// verdicts of runs already in flight rather than dropping them.
pub(crate) const INSPECTION_PROOF_KEY: &str = "inspection_proof_required";

/// Whether `prepared` was admitted under the proof contract.
pub(crate) fn inspection_proof_required(prepared: &Value) -> bool {
    prepared.get(INSPECTION_PROOF_KEY).and_then(Value::as_bool) == Some(true)
}

/// Why an inspection result's observed-revision proof does not establish
/// that it read the pinned source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InspectionProofError {
    missing: bool,
    message: String,
}

impl InspectionProofError {
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for InspectionProofError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Check an inspection result's observed-revision proof, or `None` when it
/// establishes that the result read the pinned source. `observed_revision`
/// is the `HEAD` the result reports from its checkout; `observed_tree` is
/// that checkout's `HEAD^{tree}`, checked against the tree of the pinned
/// `source_revision` recomputed host-side. Hex case is normalised, as Git
/// prints lowercase. With `required` false (a checkpoint prepared before
/// the proof existed, see [`INSPECTION_PROOF_KEY`]) a result that reports
/// neither value passes unchecked. The expected tree is never named in the
/// message: the pilot repair loop feeds these errors back to the agent,
/// which must re-derive the tree from its checkout, not echo it.
pub(crate) fn inspection_proof_error(
    required: bool,
    observed_revision: Option<&str>,
    observed_tree: Option<&str>,
    source_revision: &str,
    source_tree: &str,
) -> Option<InspectionProofError> {
    let reported = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
    };
    let observed_revision = reported(observed_revision);
    let observed_tree = reported(observed_tree);
    if !required && observed_revision.is_none() && observed_tree.is_none() {
        return None;
    }
    let (Some(observed_revision), Some(observed_tree)) = (observed_revision, observed_tree) else {
        return Some(InspectionProofError {
            missing: true,
            message: "inspection proof missing: report observed_revision (`git rev-parse HEAD`) \
                      and observed_tree (`git rev-parse 'HEAD^{tree}'`) from the inspection \
                      checkout"
                .to_string(),
        });
    };
    if observed_revision != source_revision.to_ascii_lowercase() {
        return Some(InspectionProofError {
            missing: false,
            message: format!(
                "inspection proof mismatch: observed_revision {observed_revision} differs from \
                 the pinned source_revision {source_revision}"
            ),
        });
    }
    if observed_tree != source_tree.to_ascii_lowercase() {
        return Some(InspectionProofError {
            missing: false,
            message: "inspection proof mismatch: observed_tree is not the tree of the pinned \
                      source_revision; report `git rev-parse 'HEAD^{tree}'` from the inspection \
                      checkout"
                .to_string(),
        });
    }
    None
}

pub(crate) fn resolve_source_snapshot(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    workspace_root: &Path,
) -> Result<Option<SourceSnapshot>, DispatchError> {
    if !is_git_work_tree(action, workspace_root)? {
        return Ok(None);
    }

    let base_branch = requested_base_branch(runtime, input);
    let base_branch = normalize_base_branch(action, &base_branch)?;
    if let Some(revision) = input
        .get("source_revision")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        if !is_commit_id(revision) {
            return Err(DispatchError::action_failed(
                action,
                "source_revision must be a full commit ID",
            ));
        }
        let resolved = rev_parse_commit(action, workspace_root, revision)?;
        return Ok(Some(SourceSnapshot {
            base_branch,
            source_ref: revision.into(),
            source_revision: resolved,
            fast_forwarded: false,
        }));
    }
    let has_origin = has_origin_remote(action, workspace_root)?;

    // The pin is the commit this run's own fetch read from origin, so it is
    // as fresh as fetching under the shared lock was, but the network wait
    // no longer queues behind (or holds up) other Orbit fetches [ORB-21834].
    // `refs/remotes/origin/*` is still written only under the shared
    // git-common-dir lock (`orbit-git-fetch`), the identity delivery
    // `fetch_remote_base` uses; see `publish_remote_tracking_ref`.
    let (source_ref, source_revision) = if has_origin {
        let revision =
            fetch_origin_branch(action, workspace_root, &base_branch, GIT_REMOTE_TIMEOUT)?;
        publish_remote_tracking_ref(workspace_root, &base_branch, &revision);
        (format!("origin/{base_branch}"), revision)
    } else {
        let source_ref = format!("refs/heads/{base_branch}");
        // Preserve the workspace-local lock-path probe without waiting on a
        // fetch that does not affect this local branch read.
        let revision = match try_with_git_fetch_lock(workspace_root, || {
            Ok::<_, FetchLockError>(rev_parse_commit(action, workspace_root, &source_ref))
        }) {
            Ok(Some(revision)) => revision?,
            Ok(None) => rev_parse_commit(action, workspace_root, &source_ref)?,
            Err(error) => {
                tracing::debug!(%error, "could not probe the fetch lock for a local source branch");
                rev_parse_commit(action, workspace_root, &source_ref)?
            }
        };
        (source_ref, revision)
    };

    Ok(Some(SourceSnapshot {
        base_branch,
        source_ref,
        source_revision,
        fast_forwarded: false,
    }))
}

pub(crate) fn requested_base_branch(runtime: &OrbitRuntime, input: &Value) -> String {
    input
        .get("base_branch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| runtime.workspace_base_branch().to_string())
}

fn normalize_base_branch(action: &str, base: &str) -> Result<String, DispatchError> {
    let branch = base
        .trim()
        .strip_prefix("origin/")
        .unwrap_or_else(|| base.trim())
        .trim();
    if branch.is_empty() {
        return Err(DispatchError::action_failed(
            action,
            "base_branch must be a non-empty branch name",
        ));
    }
    if branch.starts_with('-') {
        return Err(DispatchError::action_failed(
            action,
            format!("base_branch {base:?} must not start with '-'"),
        ));
    }
    Ok(branch.to_string())
}
