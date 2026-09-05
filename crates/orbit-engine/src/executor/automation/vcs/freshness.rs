use std::path::Path;

use orbit_common::{OrbitError, RecoverableVcsConflict};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::input::{input_string_field, required_input_string};
use super::git::{BaseSyncMode, git_command_success, git_output, resolve_worktree_start_point};
use super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, rebase_in_progress,
    record_failed_handoff,
};

#[derive(Debug, Clone)]
pub(super) struct BranchFreshness {
    pub(super) base_ref: String,
    pub(super) head_ref: String,
    pub(super) commits_behind: u64,
    pub(super) commits_ahead: u64,
}

pub(in crate::executor::automation) fn prepare_pr_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "pr_prepare")?;
    match prepare_pr_handoff_inner(input, &context) {
        Ok(output) => Ok(output),
        Err((phase, error)) => {
            record_failed_handoff(host, &context, input, phase, &error)?;
            Err(error)
        }
    }
}

fn prepare_pr_handoff_inner(
    input: &Value,
    context: &HandoffContext,
) -> Result<Value, (FailedHandoffPhase, OrbitError)> {
    let head = git_output(
        &context.workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .map_err(prepare_error)?
    .trim()
    .to_string();
    if head == "HEAD" {
        return Err((
            FailedHandoffPhase::Prepare,
            OrbitError::Execution("pr_prepare: workspace is in detached HEAD state".to_string()),
        ));
    }
    let head_sha = commit_sha(&context.workspace_path, &head).map_err(prepare_error)?;
    let base = input_string_field(input, "base").unwrap_or_else(|| "main".to_string());
    let sync_mode = super::git::base_sync_mode_from_input(input).map_err(prepare_error)?;
    let base_ref = resolve_worktree_start_point(&context.workspace_path, &base, sync_mode)
        .map_err(prepare_error)?;
    let base_sha = commit_sha(&context.workspace_path, &base_ref).map_err(prepare_error)?;
    let freshness =
        branch_freshness_against_ref(&context.workspace_path, &head, &base_ref, &base_sha)
            .map_err(prepare_error)?;
    if freshness.commits_ahead == 0 {
        return Err((
            FailedHandoffPhase::EmptyBranch,
            OrbitError::Execution(format!(
                "pr_prepare: head '{head}' has 0 commits ahead of base '{base}' (base checkpoint '{base_sha}'); refusing an empty PR handoff"
            )),
        ));
    }
    let remote_sha = remote_branch_sha(&context.workspace_path, &head).map_err(prepare_error)?;
    let sync_required = freshness.commits_behind > 0;
    Ok(json!({
        "phase": "prepare",
        "decision": if sync_required { "rebase_required" } else { "already_fresh" },
        "head": head,
        "head_sha": head_sha,
        "base": base,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "remote_sha": remote_sha,
        "commits_behind": freshness.commits_behind,
        "commits_ahead": freshness.commits_ahead,
        "sync_required": sync_required,
    }))
}

fn prepare_error(error: OrbitError) -> (FailedHandoffPhase, OrbitError) {
    (FailedHandoffPhase::Prepare, error)
}

pub(in crate::executor::automation) fn rebase_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "git_rebase")?;
    match rebase_pr_branch_inner(input, &context) {
        Ok(output) => Ok(output),
        Err(error) => {
            record_failed_handoff(host, &context, input, FailedHandoffPhase::Rebase, &error)?;
            Err(error)
        }
    }
}

fn rebase_pr_branch_inner(input: &Value, context: &HandoffContext) -> Result<Value, OrbitError> {
    let head = required_input_string(input, "head")?;
    let head_sha_before = required_input_string(input, "head_sha")?;
    let base = required_input_string(input, "base")?;
    let base_ref = required_input_string(input, "base_ref")?;
    let base_sha = required_input_string(input, "base_sha")?;
    let sync_required = input
        .get("sync_required")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            OrbitError::InvalidInput("missing required input.sync_required".to_string())
        })?;
    let prepared_behind = input
        .get("commits_behind")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            OrbitError::InvalidInput("missing required input.commits_behind".to_string())
        })?;
    if sync_required != (prepared_behind > 0) {
        return Err(OrbitError::InvalidInput(
            "git_rebase: sync_required disagrees with the prepared divergence checkpoint"
                .to_string(),
        ));
    }
    let current_branch = git_output(
        &context.workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    if rebase_in_progress(&context.workspace_path)? {
        let conflicting_paths = unmerged_paths(&context.workspace_path)?;
        if conflicting_paths.is_empty() {
            return Err(OrbitError::Execution(
                "git_rebase: rebase remains in progress without unresolved conflict entries"
                    .to_string(),
            ));
        }
        return Err(rebase_conflict_error(
            &context.workspace_path,
            head_sha_before,
            base_sha,
            conflicting_paths,
            "rebase remains stopped with unresolved conflicts",
        )?);
    }
    if current_branch.trim() != head {
        return Err(OrbitError::Execution(format!(
            "git_rebase: prepared branch '{head}' is not checked out (found '{}')",
            current_branch.trim()
        )));
    }
    if !git_command_success(
        &context.workspace_path,
        &["diff", "--quiet", "--diff-filter=U"],
    )? {
        return Err(rebase_conflict_error(
            &context.workspace_path,
            head_sha_before,
            base_sha,
            unmerged_paths(&context.workspace_path)?,
            "unresolved merge conflicts remain",
        )?);
    }

    let observed_base_sha = commit_sha(&context.workspace_path, base_ref)?;
    if observed_base_sha != base_sha {
        return Err(OrbitError::Execution(format!(
            "git_rebase: prepared base ref '{base_ref}' moved from checkpoint '{base_sha}' to '{observed_base_sha}'; refusing to lose concurrent base changes — prepare a fresh handoff checkpoint"
        )));
    }

    let current_sha = commit_sha(&context.workspace_path, head)?;
    let current = branch_freshness_against_ref(&context.workspace_path, head, base_ref, base_sha)?;
    if current.commits_ahead == 0 {
        return Err(OrbitError::Execution(format!(
            "git_rebase: recovered branch '{head}' no longer contains a candidate ahead of target base '{base_sha}'"
        )));
    }
    let (decision, rewritten, head_sha) = if current.commits_behind == 0 {
        if current_sha == head_sha_before {
            if sync_required {
                return Err(OrbitError::Execution(
                    "git_rebase: branch is unexpectedly fresh without changing the recorded pre-rewrite HEAD"
                        .to_string(),
                ));
            }
            ("skipped_current", false, current_sha)
        } else if sync_required {
            ("reused_recovery", true, current_sha)
        } else {
            return Err(OrbitError::Execution(format!(
                "git_rebase: branch HEAD changed from prepared checkpoint '{head_sha_before}' to '{current_sha}' without a recorded rewrite decision"
            )));
        }
    } else {
        if !sync_required || current_sha != head_sha_before {
            return Err(OrbitError::Execution(
                "git_rebase: branch state no longer matches the durable pre-rewrite checkpoint"
                    .to_string(),
            ));
        }
        if !git_command_success(&context.workspace_path, &["rebase", base_sha])? {
            let conflicting_paths = unmerged_paths(&context.workspace_path)?;
            if conflicting_paths.is_empty() {
                return Err(OrbitError::Execution(format!(
                    "git_rebase: rebase of '{head}' onto checkpoint '{base_sha}' failed without unresolved conflict entries"
                )));
            }
            return Err(rebase_conflict_error(
                &context.workspace_path,
                head_sha_before,
                base_sha,
                conflicting_paths,
                &format!("rebase of '{head}' onto checkpoint '{base_sha}' stopped with conflicts"),
            )?);
        }
        let after =
            branch_freshness_against_ref(&context.workspace_path, head, base_ref, base_sha)?;
        if after.commits_behind != 0 {
            return Err(OrbitError::Execution(
                "git_rebase: branch remains behind the recorded base after rebase".to_string(),
            ));
        }
        (
            "performed",
            true,
            commit_sha(&context.workspace_path, head)?,
        )
    };

    Ok(json!({
        "phase": "rebase",
        "decision": decision,
        "head": head,
        "head_sha": head_sha,
        "head_sha_before": head_sha_before,
        "base": base,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "remote_sha_before": input_string_field(input, "remote_sha"),
        "rewritten": rewritten,
    }))
}

fn unmerged_paths(repo_root: &Path) -> Result<Vec<String>, OrbitError> {
    Ok(
        git_output(repo_root, &["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn rebase_conflict_error(
    repo_root: &Path,
    head_sha_before: &str,
    target_base_sha: &str,
    conflicting_paths: Vec<String>,
    diagnostic: &str,
) -> Result<OrbitError, OrbitError> {
    Ok(OrbitError::RecoverableVcsConflict(Box::new(
        RecoverableVcsConflict {
            operation: "git_rebase".to_string(),
            original_base_sha: original_base_sha(repo_root, head_sha_before, target_base_sha)?,
            target_base_sha: target_base_sha.to_string(),
            conflicting_paths,
            diagnostic: diagnostic.to_string(),
        },
    )))
}

pub(super) fn original_base_sha(
    workspace_path: &Path,
    head_sha: &str,
    target_base_sha: &str,
) -> Result<String, OrbitError> {
    match git_output(workspace_path, &["merge-base", head_sha, target_base_sha]) {
        Ok(sha) if !sha.trim().is_empty() => Ok(sha.trim().to_string()),
        _ => Ok(
            git_output(workspace_path, &["rev-parse", &format!("{head_sha}^")])?
                .trim()
                .to_string(),
        ),
    }
}

pub(super) fn ensure_branch_fresh_against_base(
    repo_root: &Path,
    head: &str,
    base: &str,
    sync_mode: BaseSyncMode,
) -> Result<BranchFreshness, OrbitError> {
    let base_ref = resolve_worktree_start_point(repo_root, base, sync_mode)?;
    let base_sha = commit_sha(repo_root, &base_ref)?;
    let freshness = branch_freshness_against_ref(repo_root, head, &base_ref, &base_sha)?;

    if freshness.commits_behind > 0 {
        return Err(OrbitError::Execution(format!(
            "task branch '{head}' is behind base '{base_ref}' by {} commit(s); refresh the task branch before opening or merging the PR",
            freshness.commits_behind
        )));
    }
    Ok(freshness)
}

pub(super) fn branch_freshness_against_ref(
    repo_root: &Path,
    head: &str,
    base_ref: &str,
    base_sha: &str,
) -> Result<BranchFreshness, OrbitError> {
    let divergence = git_output(
        repo_root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base_sha}...{head}"),
        ],
    )?;
    let mut parts = divergence.split_whitespace();
    let commits_behind = parse_divergence_count(parts.next(), "behind", base_ref, head)?;
    let commits_ahead = parse_divergence_count(parts.next(), "ahead", base_ref, head)?;
    if parts.next().is_some() {
        return Err(OrbitError::Execution(format!(
            "unexpected git divergence output while comparing '{head}' to '{base_sha}': {divergence}"
        )));
    }
    Ok(BranchFreshness {
        base_ref: base_ref.to_string(),
        head_ref: head.to_string(),
        commits_behind,
        commits_ahead,
    })
}

pub(super) fn commit_sha(repo_root: &Path, reference: &str) -> Result<String, OrbitError> {
    Ok(git_output(
        repo_root,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )?
    .trim()
    .to_string())
}

pub(super) fn remote_branch_sha(
    repo_root: &Path,
    branch: &str,
) -> Result<Option<String>, OrbitError> {
    let output = git_output(
        repo_root,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let sha = output.split_whitespace().next().map(ToOwned::to_owned);
    Ok(sha.filter(|value| !value.is_empty()))
}

fn parse_divergence_count(
    value: Option<&str>,
    label: &str,
    base: &str,
    head: &str,
) -> Result<u64, OrbitError> {
    let raw = value.ok_or_else(|| {
        OrbitError::Execution(format!(
            "missing {label} divergence count while comparing '{head}' to '{base}'"
        ))
    })?;
    raw.parse::<u64>().map_err(|error| {
        OrbitError::Execution(format!(
            "invalid {label} divergence count '{raw}' while comparing '{head}' to '{base}': {error}"
        ))
    })
}
