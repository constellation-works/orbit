use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::input::{canonicalize_existing_dir, input_string_field, required_job_run_id};
use super::freshness::{commit_sha, remote_branch_sha};
use super::git::{git_command_success, git_output, git_success};
use super::handoff::{FailedHandoffPhase, load_handoff_context, record_failed_handoff};
use super::operations;

pub(in crate::executor::automation) fn push_batch_changes<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let handoff = input
        .get("completed_task_ids")
        .and_then(Value::as_array)
        .is_some_and(|ids| !ids.is_empty())
        .then(|| load_handoff_context(host, input, "git_push"))
        .transpose()?;
    let workspace_path = match handoff.as_ref() {
        Some(context) => context.workspace_path.clone(),
        None => resolve_workspace_path(host, input)?,
    };

    match push_batch_changes_inner(host, input, &workspace_path) {
        Ok(output) => Ok(output),
        Err(error) => {
            if let Some(context) = handoff.as_ref() {
                record_failed_handoff(host, context, input, FailedHandoffPhase::Push, &error)?;
            }
            Err(error)
        }
    }
}

pub(super) fn push_batch_changes_inner<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = input_string_field(input, "branch")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap_or_else(|_| "HEAD".to_string())
                .trim()
                .to_string()
        });
    if branch == "HEAD" {
        return Err(OrbitError::Execution(
            "push_batch_changes: workspace is in detached HEAD state".to_string(),
        ));
    }

    ensure_origin_publishes_elsewhere(workspace_path, &branch)?;

    let local_sha = commit_sha(workspace_path, &branch)?;
    let remote_sha = remote_branch_sha(workspace_path, &branch)?;
    let decision = push_decision(workspace_path, &branch, &local_sha, remote_sha.as_deref())?;
    let (label, force_with_lease) = match decision {
        PushDecision::Missing => ("performed_create", false),
        PushDecision::Current => {
            return Ok(push_output(
                "reused_current",
                &branch,
                &local_sha,
                remote_sha.as_deref(),
                false,
            ));
        }
        PushDecision::FastForward => ("performed_fast_forward", false),
        PushDecision::RemoteAhead => {
            return Err(OrbitError::Execution(format!(
                "git_push: remote branch 'origin/{branch}' contains commits not present in local '{branch}'; refusing to overwrite remote-only history"
            )));
        }
        PushDecision::Diverged => {
            validate_rewrite_checkpoint(input, &branch, &local_sha, remote_sha.as_deref())?;
            ("performed_force_with_lease", true)
        }
    };

    let mut tool_input = json!({
        "repo_root": workspace_path.to_string_lossy().to_string(),
        "branch": branch,
        "force_with_lease": force_with_lease,
    });
    if force_with_lease {
        tool_input["expected_remote_sha"] = json!(remote_sha);
    }
    host.run_private_vcs_operation(operations::PUSH, tool_input)?;

    Ok(push_output(
        label,
        &branch,
        &local_sha,
        remote_sha.as_deref(),
        force_with_lease,
    ))
}

/// Refuse to push into the repository we are pushing from.
///
/// A push step proves publication only when `origin` is some other repository.
/// When `origin` points back at this checkout — the primary checkout a linked
/// worktree shares its `.git` with, for instance — `ls-remote` echoes the local
/// branch straight back, and the step would report `reused_current` for a branch
/// that reached no publication target at all. Failing here keeps a failed push
/// distinguishable from a successful one even if something outside the pipeline
/// wrote that remote into the repository's configuration [ORB-12103].
fn ensure_origin_publishes_elsewhere(
    workspace_path: &Path,
    branch: &str,
) -> Result<(), OrbitError> {
    let url = git_output(workspace_path, &["remote", "get-url", "origin"]).map_err(|error| {
        OrbitError::Execution(format!(
            "git_push: workspace '{}' has no usable 'origin' remote to publish '{branch}' to: {error}",
            workspace_path.display()
        ))
    })?;
    let Some(local_remote) = local_remote_path(workspace_path, &url) else {
        return Ok(());
    };
    let (Some(remote_repository), Some(local_repository)) = (
        repository_identity(&local_remote),
        repository_identity(workspace_path),
    ) else {
        return Ok(());
    };
    if remote_repository != local_repository {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "git_push: remote 'origin' ('{url}') is this same repository ('{}'), so pushing '{branch}' \
         there publishes nothing; point 'origin' at a real publication remote",
        local_repository.display()
    )))
}

/// The filesystem path `url` names, when it names one. A transport URL or an
/// `scp`-style address resolves to no existing directory and yields `None`.
fn local_remote_path(workspace_path: &Path, url: &str) -> Option<PathBuf> {
    let candidate = url.strip_prefix("file://").unwrap_or(url);
    let resolved = workspace_path.join(candidate);
    resolved.is_dir().then_some(resolved)
}

/// The repository a path belongs to, identified by its common Git directory so
/// that a linked worktree and its primary checkout compare equal.
fn repository_identity(path: &Path) -> Option<PathBuf> {
    let common_dir = git_output(path, &["rev-parse", "--git-common-dir"]).ok()?;
    let resolved = path.join(common_dir);
    Some(std::fs::canonicalize(&resolved).unwrap_or(resolved))
}

fn resolve_workspace_path<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path"),
        None => {
            let batch_id = required_job_run_id(input, "git_push")?;
            let repo_root = host.repo_root()?;
            super::worktree::resolve_shared_worktree_path(Path::new(&repo_root), batch_id)
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PushDecision {
    Missing,
    Current,
    FastForward,
    RemoteAhead,
    Diverged,
}

fn push_decision(
    workspace_path: &Path,
    branch: &str,
    local_sha: &str,
    remote_sha: Option<&str>,
) -> Result<PushDecision, OrbitError> {
    let Some(remote_sha) = remote_sha else {
        return Ok(PushDecision::Missing);
    };
    if remote_sha == local_sha {
        return Ok(PushDecision::Current);
    }

    git_success(
        workspace_path,
        &[
            "fetch",
            "--no-tags",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    if git_command_success(
        workspace_path,
        &["merge-base", "--is-ancestor", remote_sha, local_sha],
    )? {
        return Ok(PushDecision::FastForward);
    }
    if git_command_success(
        workspace_path,
        &["merge-base", "--is-ancestor", local_sha, remote_sha],
    )? {
        return Ok(PushDecision::RemoteAhead);
    }
    Ok(PushDecision::Diverged)
}

fn validate_rewrite_checkpoint(
    input: &Value,
    branch: &str,
    local_sha: &str,
    observed_remote_sha: Option<&str>,
) -> Result<(), OrbitError> {
    // Divergence is safe only with the exact durable pre-rewrite remote SHA.
    let rewritten = input
        .get("rewrite_performed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head_before = input_string_field(input, "rewrite_head_before");
    let expected_remote_sha = input_string_field(input, "expected_remote_sha");
    let Some(observed_remote_sha) = observed_remote_sha else {
        return Err(OrbitError::Execution(format!(
            "git_push: divergent decision for '{branch}' has no observed remote SHA"
        )));
    };
    if !rewritten
        || head_before.as_deref() == Some(local_sha)
        || expected_remote_sha.as_deref() != Some(observed_remote_sha)
    {
        return Err(OrbitError::Execution(format!(
            "git_push: branch 'origin/{branch}' diverged, but no durable rewrite checkpoint authorizes replacing exact remote SHA '{observed_remote_sha}'"
        )));
    }
    Ok(())
}

fn push_output(
    decision: &str,
    branch: &str,
    local_sha: &str,
    remote_sha: Option<&str>,
    force_with_lease: bool,
) -> Value {
    json!({
        "phase": "push",
        "decision": decision,
        "branch": branch,
        "local_sha": local_sha,
        "remote_sha_before": remote_sha,
        "force_with_lease": force_with_lease,
    })
}
