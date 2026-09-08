//! Landing-branch source snapshot for task-pilot prepare/apply.
//!
//! Inspection and deterministic selector validation share one pinned revision
//! so a newly merged target is not reported as missing when the primary
//! checkout lags origin. Fetch updates only remote refs/objects; the primary
//! HEAD, index and working files are never aligned or otherwise modified.

use std::io;
use std::path::Path;
use std::process::Command;

use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, git_fetch_cas_retry_delay, run_git, should_retry_git_ref_cas,
    with_git_fetch_lock,
};
use orbit_engine::DispatchError;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::action_failed;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceSnapshot {
    pub base_branch: String,
    pub source_ref: String,
    pub source_revision: String,
    // Retained in prepared checkpoints for compatibility with earlier runs.
    // New preparations never move the primary and always emit false.
    pub fast_forwarded: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GitPathKind {
    Blob,
    Tree,
    Missing,
    Other,
}

impl SourceSnapshot {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "base_branch": self.base_branch,
            "source_ref": self.source_ref,
            "source_revision": self.source_revision,
            "fast_forwarded": self.fast_forwarded,
        })
    }

    pub(super) fn from_prepared(
        prepared: &Value,
        action: &str,
    ) -> Result<Option<Self>, DispatchError> {
        let Some(source) = prepared.get("source") else {
            return Ok(None);
        };
        if source.is_null() {
            return Ok(None);
        }
        let object = source
            .as_object()
            .ok_or_else(|| action_failed(action, "prepared.source must be an object or null"))?;
        let source_revision = object
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(source_revision) = source_revision else {
            return Ok(None);
        };
        if !is_commit_id(source_revision) {
            return Err(action_failed(
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
                action_failed(
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
                action_failed(
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

    pub(super) fn ensure_commit(
        &self,
        action: &str,
        workspace: &Path,
    ) -> Result<(), DispatchError> {
        let spec = format!("{}^{{commit}}", self.source_revision);
        let output = git(action, workspace, &["rev-parse", "--verify", &spec])?;
        if output.success {
            Ok(())
        } else {
            Err(action_failed(
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

    pub(super) fn path_kind(
        &self,
        action: &str,
        workspace: &Path,
        anchor: &Path,
    ) -> Result<GitPathKind, DispatchError> {
        let relative = git_tree_path(anchor).ok_or_else(|| {
            action_failed(
                action,
                format!(
                    "selector anchor {} is not a repository-relative path",
                    anchor.display()
                ),
            )
        })?;
        let output = git(
            action,
            workspace,
            &[
                "ls-tree",
                "--format=%(objectmode) %(objecttype)",
                &self.source_revision,
                "--",
                &format!(":(literal){relative}"),
            ],
        )?;
        if !output.success {
            return Err(action_failed(
                action,
                format!("read pinned source tree: {}", output.stderr.trim()),
            ));
        }
        Ok(match output.stdout.trim() {
            "100644 blob" | "100755 blob" => GitPathKind::Blob,
            "040000 tree" => GitPathKind::Tree,
            "" => GitPathKind::Missing,
            _ => GitPathKind::Other,
        })
    }
}

pub(super) fn resolve_source_snapshot(
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
            return Err(action_failed(
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
            fetch_origin_branch(action, workspace_root, &base_branch)
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
        FetchLockError::Io(error) => action_failed(action, format!("git fetch lock: {error}")),
    })?;

    Ok(Some(SourceSnapshot {
        base_branch,
        source_ref,
        source_revision,
        fast_forwarded: false,
    }))
}

pub(super) fn requested_base_branch(runtime: &OrbitRuntime, input: &Value) -> String {
    input
        .get("base_branch")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| runtime.workflow_base_branch().to_string())
}

fn normalize_base_branch(action: &str, base: &str) -> Result<String, DispatchError> {
    let branch = base
        .trim()
        .strip_prefix("origin/")
        .unwrap_or_else(|| base.trim())
        .trim();
    if branch.is_empty() {
        return Err(action_failed(
            action,
            "base_branch must be a non-empty branch name",
        ));
    }
    if branch.starts_with('-') {
        return Err(action_failed(
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

fn fetch_origin_branch(action: &str, workspace: &Path, branch: &str) -> Result<(), DispatchError> {
    let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    let mut last_stderr = String::new();
    for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
        let output = Command::new("git")
            .args(["fetch", "origin", &spec])
            .current_dir(workspace)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|error| {
                action_failed(
                    action,
                    format!(
                        "failed to fetch origin/{branch} in '{}': {error}",
                        workspace.display()
                    ),
                )
            })?;
        if output.status.success() {
            return Ok(());
        }
        last_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
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
    Err(action_failed(
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
        return Err(action_failed(
            action,
            format!(
                "unable to resolve source revision `{rev}`: {}",
                output.stderr.trim()
            ),
        ));
    }
    let sha = output.stdout.trim();
    if !is_commit_id(sha) {
        return Err(action_failed(
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

fn is_commit_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn git(
    action: &str,
    workspace: &Path,
    args: &[&str],
) -> Result<orbit_common::fs::git::GitCommandOutput, DispatchError> {
    run_git(workspace, args).map_err(|error| {
        action_failed(
            action,
            format!(
                "git {} failed in '{}': {error}",
                args.join(" "),
                workspace.display()
            ),
        )
    })
}
