//! PR handoff preparation and deterministic branch rebasing.
use super::super::super::input::{input_string_field, required_input_string};
use super::super::absorbed::{AbsorbedReason, AbsorbedRefusal, verify_absorbed_candidate};
use super::super::git::{
    GitTimeoutBudget, GitTimeoutBudgetGuard, git_command_success, git_failure_error, git_output,
    git_run, git_success, resolve_worktree_start_point,
};
use super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, rebase_in_progress,
    record_failed_handoff,
};
use super::checkpoint::recovery_run_id;
use super::divergence::{branch_freshness_against_ref, commit_sha, remote_branch_sha};
use super::rebase_recovery::{
    RecoveredRewrite, ensure_clean_for_rewrite, rebase_conflict_error,
    recover_started_rebase_timeout, recovered_rewrite, refuse_or_recover_existing_rebase,
    unmerged_paths,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use serde_json::{Value, json};
use std::path::Path;

pub(in crate::executor::automation) fn prepare_pr_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "pr_prepare")?;
    match prepare_pr_handoff_inner(input, &context) {
        Ok(output) => Ok(output),
        Err(failure) => {
            let (phase, error) = *failure;
            record_failed_handoff(host, &context, input, phase, &error)?;
            Err(error)
        }
    }
}

fn prepare_pr_handoff_inner(
    input: &Value,
    context: &HandoffContext,
) -> Result<Value, Box<(FailedHandoffPhase, OrbitError)>> {
    let head = git_output(
        &context.workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )
    .map_err(prepare_error)?
    .trim()
    .to_string();
    if head == "HEAD" {
        return Err(Box::new((
            FailedHandoffPhase::Prepare,
            OrbitError::Execution("pr_prepare: workspace is in detached HEAD state".to_string()),
        )));
    }
    let head_sha = commit_sha(&context.workspace_path, &head).map_err(prepare_error)?;
    let base = input_string_field(input, "base").unwrap_or_else(|| "main".to_string());
    let sync_mode = super::super::git::base_sync_mode_from_input(input).map_err(prepare_error)?;
    let base_ref = resolve_worktree_start_point(&context.workspace_path, &base, sync_mode)
        .map_err(prepare_error)?;
    let base_sha = commit_sha(&context.workspace_path, &base_ref).map_err(prepare_error)?;
    let freshness =
        branch_freshness_against_ref(&context.workspace_path, &head, &base_ref, &base_sha)
            .map_err(prepare_error)?;
    if freshness.commits_ahead == 0 {
        return Err(Box::new((
            FailedHandoffPhase::EmptyBranch,
            OrbitError::Execution(format!(
                "pr_prepare: head '{head}' has 0 commits ahead of base '{base}' (base checkpoint '{base_sha}'); refusing an empty PR handoff"
            )),
        )));
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

fn prepare_error(error: OrbitError) -> Box<(FailedHandoffPhase, OrbitError)> {
    Box::new((FailedHandoffPhase::Prepare, error))
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

    let current_sha = commit_sha(&context.workspace_path, head)?;
    // A rewritten HEAD is judged by its host-certified recovery checkpoint,
    // looked up once here. That checkpoint also names the base the host
    // actually landed the candidate on: conflict recovery continues the
    // stopped rebase at the prepared pin and then follows a base that
    // advanced past it, so the retry must judge freshness against that
    // landed base rather than the stale pin.
    let recovery = if sync_required && current_sha != head_sha_before {
        recovered_rewrite(host, input, context, &current_sha)?
    } else {
        RecoveredRewrite::Absent
    };
    // [ORB-14668] A certified absorbed continuation left nothing to deliver,
    // whatever the base did since: re-observe it and hand the step to
    // verified no-diff settlement, or name why it is refused.
    if let RecoveredRewrite::Current(checkpoint) = &recovery
        && checkpoint.get("absorbed").is_some()
    {
        return Err(absorbed_outcome(host, input, context, head, checkpoint));
    }
    let base_sha = match &recovery {
        RecoveredRewrite::Current(checkpoint) => required_input_string(checkpoint, "base_sha")?,
        RecoveredRewrite::Overtaken(_)
        | RecoveredRewrite::Uncertified
        | RecoveredRewrite::Absent => base_sha,
    };

    let observed_base_sha = commit_sha(&context.workspace_path, base_ref)?;
    if observed_base_sha != base_sha {
        return Err(OrbitError::Execution(format!(
            "git_rebase: prepared base ref '{base_ref}' moved from checkpoint '{base_sha}' to '{observed_base_sha}'; refusing to lose concurrent base changes — prepare a fresh handoff checkpoint"
        )));
    }
    if let RecoveredRewrite::Overtaken(checkpoint) = &recovery {
        return super::super::base_chase::chase_advanced_base(
            host,
            input,
            context,
            checkpoint,
            &current_sha,
        );
    }

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
            match recovery {
                RecoveredRewrite::Current(_) => ("reused_recovery", true, current_sha),
                RecoveredRewrite::Uncertified => {
                    // No host-certified evidence justifies inheriting this
                    // changed HEAD (a pre-authority-boundary checkpoint and a
                    // forged one look identical, and neither is trusted).
                    // Discard it and redo the rebase from the durable
                    // pre-rewrite checkpoint instead: a clean redo produces a
                    // freshly self-verified HEAD, and a redo that hits
                    // conflicts falls into the ordinary supported
                    // conflict-recovery path, which certifies fresh evidence
                    // on completion.
                    discard_rewrite(
                        &context.workspace_path,
                        head_sha_before,
                        "an unauthenticated rewritten HEAD",
                    )?;
                    perform_rebase_onto_base(
                        &context.workspace_path,
                        head,
                        head_sha_before,
                        base_ref,
                        base_sha,
                    )?
                }
                // Dispatched to the chase above, after the base check.
                RecoveredRewrite::Overtaken(_) | RecoveredRewrite::Absent => {
                    return Err(OrbitError::Execution(
                        "git_rebase: changed HEAD has no exact host-validated recovery checkpoint"
                            .to_string(),
                    ));
                }
            }
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
        perform_rebase_onto_base(
            &context.workspace_path,
            head,
            head_sha_before,
            base_ref,
            base_sha,
        )?
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

/// The step failure for a recovered HEAD whose certified checkpoint says the
/// pinned base absorbed the candidate: the settlement marker once
/// [`verify_absorbed_candidate`] re-observes it, otherwise today's empty-branch
/// refusal with the typed reason.
fn absorbed_outcome<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    head: &str,
    checkpoint: &Value,
) -> OrbitError {
    let step_id = checkpoint
        .get("step_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let verified = match context.tasks.as_slice() {
        [task] => verify_absorbed_candidate(
            host,
            recovery_run_id(input, context),
            step_id,
            &task.id,
            &context.workspace_path,
        ),
        _ => Err(AbsorbedRefusal::new(
            AbsorbedReason::StaleCheckpoint,
            "an absorbed candidate settles exactly one task",
        )),
    };
    match verified {
        Ok(candidate) => OrbitError::Execution(candidate.failure_text(head)),
        Err(refusal) => OrbitError::Execution(format!(
            "git_rebase: recovered branch '{head}' no longer contains a candidate ahead of target base '{}'; {refusal}",
            checkpoint
                .get("base_sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )),
    }
}

/// Run the deterministic rebase itself, from `head_sha_before` (which the
/// caller must already have made the worktree's actual HEAD) onto `base_sha`.
/// A clean result is self-verifying and needs no stored evidence; a conflict
/// routes into the existing supported conflict-recovery path via
/// [`rebase_conflict_error`].
pub(in crate::executor::automation::vcs) fn perform_rebase_onto_base(
    workspace_path: &Path,
    head: &str,
    head_sha_before: &str,
    base_ref: &str,
    base_sha: &str,
) -> Result<(&'static str, bool, String), OrbitError> {
    let rebase_outcome = git_run(workspace_path, &["rebase", base_sha])?;
    if rebase_outcome.timed_out {
        return recover_started_rebase_timeout(
            workspace_path,
            head,
            head_sha_before,
            base_sha,
            &rebase_outcome,
        );
    }
    if !rebase_outcome.success {
        let conflicting_paths = unmerged_paths(workspace_path)?;
        if conflicting_paths.is_empty() {
            return Err(git_failure_error(
                workspace_path,
                &["rebase", base_sha],
                rebase_outcome.exit_code,
                &rebase_outcome.stderr,
            ));
        }
        return Err(rebase_conflict_error(
            workspace_path,
            head_sha_before,
            base_sha,
            conflicting_paths,
            &format!("rebase of '{head}' onto checkpoint '{base_sha}' stopped with conflicts"),
        )?);
    }
    let after = branch_freshness_against_ref(workspace_path, head, base_ref, base_sha)?;
    if after.commits_behind != 0 {
        return Err(OrbitError::Execution(
            "git_rebase: branch remains behind the recorded base after rebase".to_string(),
        ));
    }
    Ok(("performed", true, commit_sha(workspace_path, head)?))
}

/// Discard a changed HEAD (`what` names it in errors), restoring the worktree
/// to the last durable pre-rewrite checkpoint so the ordinary rebase path can
/// redo the work from there.
///
/// `git reset --hard` has no concept of "safe to discard": it silently drops
/// staged changes, unstaged changes, and any untracked file or directory in
/// the way, with nothing recorded afterward. Refuse instead of resetting
/// whenever the worktree is not already clean, naming every dirty path so an
/// operator can recover the state themselves before retrying.
pub(in crate::executor::automation::vcs) fn discard_rewrite(
    workspace_path: &Path,
    head_sha_before: &str,
    what: &str,
) -> Result<(), OrbitError> {
    ensure_clean_for_rewrite(workspace_path, head_sha_before, what)?;
    git_success(workspace_path, &["reset", "--hard", head_sha_before]).map_err(|error| {
        OrbitError::Execution(format!(
            "git_rebase: failed to discard {what} before redoing the rebase from checkpoint \
             '{head_sha_before}': {error}"
        ))
    })
}
