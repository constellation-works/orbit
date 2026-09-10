use std::path::{Path, PathBuf};

use orbit_common::{OrbitError, RecoverableVcsConflict};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::input::{input_string_field, required_input_string};
use super::git::{
    BaseSyncMode, GitTimeoutBudget, GitTimeoutBudgetGuard, git_command_success, git_failure_error,
    git_output, git_run, git_success, git_timeout_error, resolve_worktree_start_point,
};
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
    let _timeout_budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget::from_input(input)?);
    let context = load_handoff_context(host, input, "git_rebase")?;
    match rebase_pr_branch_inner(host, input, &context) {
        Ok(output) => Ok(output),
        Err(error) => {
            record_failed_handoff(host, &context, input, FailedHandoffPhase::Rebase, &error)?;
            Err(error)
        }
    }
}

fn rebase_pr_branch_inner<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
) -> Result<Value, OrbitError> {
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
        return refuse_or_recover_existing_rebase(
            &context.workspace_path,
            head,
            head_sha_before,
            base_sha,
        );
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
            validate_recovered_rewrite(host, input, context, &current_sha)?;
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
        let rebase_outcome = git_run(&context.workspace_path, &["rebase", base_sha])?;
        if rebase_outcome.timed_out {
            return recover_started_rebase_timeout(
                &context.workspace_path,
                head,
                head_sha_before,
                base_sha,
                &rebase_outcome,
            );
        }
        if !rebase_outcome.success {
            let conflicting_paths = unmerged_paths(&context.workspace_path)?;
            if conflicting_paths.is_empty() {
                return Err(git_failure_error(
                    &context.workspace_path,
                    &["rebase", base_sha],
                    &rebase_outcome.stderr,
                ));
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

fn refuse_or_recover_existing_rebase(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
) -> Result<Value, OrbitError> {
    let conflicting_paths = unmerged_paths(workspace_path)?;
    if !conflicting_paths.is_empty() {
        return Err(rebase_conflict_error(
            workspace_path,
            head_sha_before,
            base_sha,
            conflicting_paths,
            "rebase remains stopped with unresolved conflicts",
        )?);
    }
    if rebase_belongs_to_attempt(workspace_path, head, head_sha_before, base_sha)? {
        abort_owned_rebase(workspace_path)?;
        return Err(OrbitError::Execution(
            "git_rebase: interrupted rebase started by this attempt was aborted after a Git timeout. Retry can start clean. This is timeout recovery, not a merge conflict and not failure-handoff recovery.".to_string(),
        ));
    }
    let provenance = rebase_provenance_summary(workspace_path);
    Err(OrbitError::Execution(format!(
        "git_rebase: a pre-existing rebase is in progress without unresolved conflict entries ({provenance}). Not aborting; foreign or retained candidate state was left intact. Inspect the worktree before retrying. This is not conflict recovery."
    )))
}

fn recover_started_rebase_timeout(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
    outcome: &super::git::GitOutcome,
) -> Result<Value, OrbitError> {
    let timeout = git_timeout_error(
        workspace_path,
        &["rebase", base_sha],
        outcome.timeout_ms,
        &outcome.stderr,
    );
    if !rebase_in_progress(workspace_path).unwrap_or(false) {
        return Err(OrbitError::Execution(format!(
            "{timeout}; git_rebase of '{head}' onto '{base_sha}' timed out. This is timeout recovery, not a merge conflict and not failure-handoff recovery."
        )));
    }
    let conflicting_paths = unmerged_paths(workspace_path).unwrap_or_default();
    if !conflicting_paths.is_empty() {
        return Err(rebase_conflict_error(
            workspace_path,
            head_sha_before,
            base_sha,
            conflicting_paths,
            &format!(
                "rebase of '{head}' onto checkpoint '{base_sha}' timed out while stopped with conflicts"
            ),
        )?);
    }
    abort_owned_rebase(workspace_path)?;
    Err(OrbitError::Execution(format!(
        "{timeout}; interrupted rebase started by this attempt was aborted. Retry can start clean. This is timeout recovery, not a merge conflict and not failure-handoff recovery."
    )))
}

fn abort_owned_rebase(workspace_path: &Path) -> Result<(), OrbitError> {
    git_success(workspace_path, &["rebase", "--abort"]).map_err(|error| {
        OrbitError::Execution(format!(
            "git_rebase: failed to abort an interrupted rebase started by this attempt: {error}"
        ))
    })
}

fn rebase_belongs_to_attempt(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_sha: &str,
) -> Result<bool, OrbitError> {
    let orig_head = read_rebase_state(workspace_path, "orig-head")?;
    let onto = read_rebase_state(workspace_path, "onto")?;
    let head_name = read_rebase_state(workspace_path, "head-name")?;
    let orig_ok = orig_head.as_deref() == Some(head_sha_before);
    let onto_ok = onto.as_deref() == Some(base_sha);
    let head_ok = head_name.as_deref().is_some_and(|name| {
        name == head || name == format!("refs/heads/{head}") || name.ends_with(&format!("/{head}"))
    });
    Ok(orig_ok && onto_ok && head_ok)
}

fn rebase_provenance_summary(workspace_path: &Path) -> String {
    let orig_head = read_rebase_state(workspace_path, "orig-head")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let onto = read_rebase_state(workspace_path, "onto")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let head_name = read_rebase_state(workspace_path, "head-name")
        .ok()
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    format!("orig-head={orig_head}, onto={onto}, head-name={head_name}")
}

fn read_rebase_state(workspace_path: &Path, name: &str) -> Result<Option<String>, OrbitError> {
    for dir in ["rebase-merge", "rebase-apply"] {
        let rel = match git_output(
            workspace_path,
            &["rev-parse", "--git-path", &format!("{dir}/{name}")],
        ) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let path = if Path::new(&rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            workspace_path.join(rel)
        };
        if let Ok(contents) = std::fs::read_to_string(&path) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_string()));
            }
        }
    }
    Ok(None)
}

fn validate_recovered_rewrite<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    current_sha: &str,
) -> Result<(), OrbitError> {
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or(&context.batch_id);
    let Some(checkpoint) =
        recovered_head_checkpoint(host, run_id, &context.workspace_path, current_sha)?
    else {
        return Err(OrbitError::Execution(
            "git_rebase: changed HEAD has no exact host-validated recovery checkpoint".to_string(),
        ));
    };
    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    if checkpoint["head"] != input["head"]
        || checkpoint["head_sha_before"] != input["head_sha"]
        || checkpoint["base_sha"] != input["base_sha"]
        || checkpoint["remote_sha_before"]
            != input.get("remote_sha").cloned().unwrap_or(Value::Null)
        || checkpoint["task_ids"] != json!(task_ids)
    {
        return Err(OrbitError::Execution(
            "git_rebase: recovered HEAD provenance does not match the prepared rewrite checkpoint"
                .to_string(),
        ));
    }
    Ok(())
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

/// Read only host-written provenance, authenticating the original durable run
/// when a resume carries a copy. Advisory activity outputs never authorize HEAD.
///
/// The run store these entries come from is writable by managed leaves, so a
/// matching entry is a candidate, not authority. Every candidate must also
/// carry the host's certificate from [`RuntimeHost::verify_rebase_recovery`].
/// A checkpoint written before that boundary existed has no certificate and is
/// refused here; the run redoes the rebase rather than inheriting an
/// unauthenticated HEAD.
pub(super) fn recovered_head_checkpoint<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    workspace: &Path,
    head_sha: &str,
) -> Result<Option<Value>, OrbitError> {
    let Some(state) = host.read_run_state(run_id)? else {
        return Ok(None);
    };
    for (step_id, checkpoint) in &state.rebase_recovery_checkpoints {
        if !matches!(step_id.as_str(), "sync_base" | "complete_pr")
            || checkpoint.get("head_sha").and_then(Value::as_str) != Some(head_sha)
            || checkpoint.get("workspace_path").and_then(Value::as_str) != workspace.to_str()
            || checkpoint.get("step_id").and_then(Value::as_str) != Some(step_id)
            || checkpoint.get("rewritten").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }
        let source_run_id = required_input_string(checkpoint, "run_id")?;
        if !host.verify_rebase_recovery(source_run_id, step_id, checkpoint)? {
            return Err(OrbitError::Execution(format!(
                "recovered rebase for step `{step_id}` of run {source_run_id} carries no host \
                 recovery certificate; rerun the rebase instead of trusting run-store evidence"
            )));
        }
        if source_run_id != run_id {
            let source = host.read_run_state(source_run_id)?.ok_or_else(|| {
                OrbitError::Execution(
                    "recovered rebase source run has no durable state".to_string(),
                )
            })?;
            if source.rebase_recovery_checkpoints.get(step_id) != Some(checkpoint) {
                return Err(OrbitError::Execution(
                    "recovered rebase differs from its source checkpoint".to_string(),
                ));
            }
            let task_id = checkpoint
                .get("task_ids")
                .and_then(Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    OrbitError::Execution("recovered rebase has no task identity".to_string())
                })?;
            super::resume::ensure_retry_descends_from(
                host,
                "rebase recovery",
                "recovery run",
                task_id,
                run_id,
                source_run_id,
            )?;
        }
        let target = required_input_string(checkpoint, "base_sha")?;
        if !git_command_success(
            workspace,
            &["merge-base", "--is-ancestor", target, head_sha],
        )? {
            return Err(OrbitError::Execution(
                "recovered candidate does not descend from its pinned base".to_string(),
            ));
        }
        return Ok(Some(checkpoint.clone()));
    }
    Ok(None)
}
