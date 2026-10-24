//! Landing-branch source snapshot for task-pilot prepare/apply.
//!
//! Inspection and deterministic selector validation share one pinned revision
//! so a newly merged target is not reported as missing when the primary
//! checkout lags origin. Fetch updates only remote refs/objects; the primary
//! HEAD, index and working files are never aligned or otherwise modified.

use std::io;
use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, GIT_REMOTE_TIMEOUT, git_fetch_cas_retry_delay, run_git, run_git_within,
    should_retry_git_ref_cas, with_git_fetch_lock,
};
use orbit_engine::DispatchError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

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
    pub(super) fn age(&self, workspace: &Path) -> Value {
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

    /// How many lines the file at repository-relative `path` holds at the
    /// pinned revision, or `None` when no file is there.
    pub(crate) fn file_line_count(
        &self,
        action: &str,
        workspace: &Path,
        path: &str,
    ) -> Result<Option<usize>, DispatchError> {
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
        Ok(Some(output.stdout.lines().count()))
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
    /// The stable reason code. `inspection_proof_missing`: the result
    /// reported no complete proof, the mark of a crew whose native tools
    /// never ran (a misconfigured executor). `inspection_revision_mismatch`:
    /// it reported a pair that is not the pinned revision's, the mark of a
    /// wrong checkout.
    pub(crate) fn reason(&self) -> &'static str {
        if self.missing {
            "inspection_proof_missing"
        } else {
            "inspection_revision_mismatch"
        }
    }

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

/// The tree id of the commit `revision` in `workspace`: `Ok(Err(stderr))`
/// when Git has no readable tree for it.
pub(crate) fn tree_of(
    workspace: &Path,
    revision: &str,
) -> Result<Result<String, String>, OrbitError> {
    let spec = format!("{revision}^{{tree}}");
    let output = run_git(workspace, &["rev-parse", "--verify", &spec])?;
    let tree = output.stdout.trim();
    if !output.success || !is_commit_id(tree) {
        return Ok(Err(output.stderr.trim().to_string()));
    }
    Ok(Ok(tree.to_string()))
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

    // Shared git-common-dir lock (`orbit-git-fetch`): the same identity
    // delivery `fetch_remote_base` uses. ORB-11269's pilot-only
    // `orbit-task-pilot-fetch` lock left that writer uncoordinated.
    let (source_ref, source_revision) = with_git_fetch_lock(workspace_root, || {
        let source_ref = if has_origin {
            fetch_origin_branch(action, workspace_root, &base_branch, GIT_REMOTE_TIMEOUT)
                .map_err(FetchLockError::Dispatch)?;
            format!("origin/{base_branch}")
        } else {
            format!("refs/heads/{base_branch}")
        };
        let revision = rev_parse_commit(action, workspace_root, &source_ref)
            .map_err(FetchLockError::Dispatch)?;
        Ok::<_, FetchLockError>((source_ref, revision))
    })
    .map_err(|error| match error {
        FetchLockError::Dispatch(error) => error,
        FetchLockError::Io(error) => {
            DispatchError::action_failed(action, format!("git fetch lock: {error}"))
        }
    })?;

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

/// Local-error wrapper so [`with_git_fetch_lock`] (which requires
/// `E: From<io::Error>`) can carry either lock-acquisition failures or the
/// module's own [`DispatchError`] out of the locked closure.
enum FetchLockError {
    Io(io::Error),
    Dispatch(DispatchError),
}

impl From<io::Error> for FetchLockError {
    fn from(error: io::Error) -> Self {
        FetchLockError::Io(error)
    }
}

fn is_git_work_tree(action: &str, workspace: &Path) -> Result<bool, DispatchError> {
    let output = git(action, workspace, &["rev-parse", "--is-inside-work-tree"])?;
    Ok(output.success && output.stdout.trim() == "true")
}

fn has_origin_remote(action: &str, workspace: &Path) -> Result<bool, DispatchError> {
    let remotes = git(action, workspace, &["remote"])?;
    if !remotes.success {
        return Ok(false);
    }
    Ok(remotes
        .stdout
        .lines()
        .map(str::trim)
        .any(|name| name == "origin"))
}

/// Fetch `origin/<branch>` with each attempt bounded by `deadline`. The
/// caller holds the shared fetch lock, so a stalled remote is killed at the
/// deadline rather than holding every other workspace fetch behind it.
pub(super) fn fetch_origin_branch(
    action: &str,
    workspace: &Path,
    branch: &str,
    deadline: Duration,
) -> Result<(), DispatchError> {
    let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    let mut last_stderr = String::new();
    for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
        let output =
            run_git_within(workspace, &["fetch", "origin", &spec], deadline).map_err(|error| {
                let failure = match error {
                    OrbitError::ProcessTimeout { .. } => "remote failure: timed out fetching",
                    _ => "failed to fetch",
                };
                DispatchError::action_failed(
                    action,
                    format!(
                        "{failure} origin/{branch} in '{}': {error}",
                        workspace.display()
                    ),
                )
            })?;
        if output.success {
            return Ok(());
        }
        last_stderr = output.stderr.trim().to_string();
        if should_retry_git_ref_cas(attempt, &last_stderr) {
            tracing::warn!(
                attempt,
                branch,
                "retrying origin fetch after git ref update contention"
            );
            std::thread::sleep(git_fetch_cas_retry_delay());
            continue;
        }
        break;
    }
    Err(DispatchError::action_failed(
        action,
        format!(
            "remote failure: could not fetch origin/{branch} in '{}': {last_stderr}",
            workspace.display(),
        ),
    ))
}

fn rev_parse_commit(action: &str, workspace: &Path, rev: &str) -> Result<String, DispatchError> {
    let spec = format!("{rev}^{{commit}}");
    let output = git(action, workspace, &["rev-parse", "--verify", &spec])?;
    if !output.success {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "unable to resolve source revision `{rev}`: {}",
                output.stderr.trim()
            ),
        ));
    }
    let sha = output.stdout.trim();
    if !is_commit_id(sha) {
        return Err(DispatchError::action_failed(
            action,
            format!("resolved `{rev}` to a non-commit id {sha:?}"),
        ));
    }
    Ok(sha.to_string())
}

fn git_tree_path(anchor: &Path) -> Option<String> {
    if anchor.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in anchor.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Whether `value` is a full SHA-1 or SHA-256 commit id, in either hex case.
pub(crate) fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn git(
    action: &str,
    workspace: &Path,
    args: &[&str],
) -> Result<orbit_common::fs::git::GitCommandOutput, DispatchError> {
    run_git(workspace, args).map_err(|error| {
        DispatchError::action_failed(
            action,
            format!(
                "git {} failed in '{}': {error}",
                args.join(" "),
                workspace.display()
            ),
        )
    })
}
