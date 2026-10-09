//! Following a base that advanced after conflict recovery [ORB-14393].
//!
//! Completion re-pins its rebase to the base it fetches, so under concurrent
//! landings the base routinely moves between a certified conflict recovery
//! and the retry that delivers it. The retry's provenance guard still judges
//! exactly the checkpoint the current attempt prepared; when that checkpoint
//! is only behind (its base an ancestor of the prepared one), this module
//! carries the recovered HEAD onto the new base and certifies the result, or
//! hands a conflicting advance to another bounded conflict recovery.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::DispatchError;
use crate::context::{RebaseRecoveryAttemptScope, RuntimeHost};

use super::super::input::{input_string_field, required_input_string};
use super::freshness::{
    abort_owned_rebase, branch_freshness_against_ref, commit_sha, discard_rewrite,
    ensure_clean_for_rewrite, original_base_sha, perform_rebase_onto_base, recovery_run_id,
    unmerged_paths,
};
use super::git::{git_failure_error, git_run, git_timeout_error, timeout_recovery_error};
use super::handoff::{HandoffContext, rebase_in_progress};

/// How many times one step may carry its certified recovery onto a base that
/// advanced past it. Every chase either rebases cleanly, which costs seconds,
/// or spends one more bounded conflict recovery; a base that keeps moving
/// past this blocks as `base_chase_exhausted` instead of looping.
const MAX_BASE_CHASES: usize = 2;

/// Carry a certified recovered HEAD onto a base that advanced after the
/// recovery certified it [ORB-14393].
///
/// `recovered` landed the prepared rewrite on the checkpoint's base, which
/// the prepared base now strictly descends from. The recovered HEAD is
/// rebased onto the prepared base; a clean result is certified as this step's
/// newest recovery attempt for that base, so a retry reuses it and the
/// provenance guard keeps judging exactly the checkpoint the current attempt
/// prepared. A conflicting advance is aborted and the rebase is redone from
/// the prepared pre-rewrite HEAD, so the ordinary bounded conflict recovery
/// can be admitted on its stopped rebase. Each chase onto a new base counts
/// against [`MAX_BASE_CHASES`] in the host's own attempt record.
pub(super) fn chase_advanced_base<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    checkpoint: &Value,
    recovered: &str,
) -> Result<Value, OrbitError> {
    let workspace = context.workspace_path.as_path();
    let head = required_input_string(input, "head")?;
    let head_sha_before = required_input_string(input, "head_sha")?;
    let base_ref = required_input_string(input, "base_ref")?;
    let base_sha = required_input_string(input, "base_sha")?;
    let run_id = recovery_run_id(input, context);
    let step_id = required_input_string(checkpoint, "step_id")?;
    let recovered_base = required_input_string(checkpoint, "base_sha")?;

    let mut bases = host
        .rebase_recovery_attempts(run_id, step_id)?
        .into_iter()
        .map(|attempt| attempt.target_base_sha)
        .collect::<std::collections::BTreeSet<_>>();
    bases.insert(recovered_base.to_string());
    bases.insert(base_sha.to_string());
    if bases.len() - 1 > MAX_BASE_CHASES {
        return Err(OrbitError::Execution(format!(
            "base_chase_exhausted: '{base_ref}' advanced to '{base_sha}' after step \
             `{step_id}` certified recovered HEAD '{recovered}' on '{recovered_base}', and the \
             step has already followed its base {MAX_BASE_CHASES} time(s); refusing to chase \
             it again"
        )));
    }

    let what = "a recovered HEAD the base advanced past";
    ensure_clean_for_rewrite(workspace, head_sha_before, what)?;
    let chased = git_run(workspace, &["rebase", base_sha])?;
    if chased.success {
        let after = branch_freshness_against_ref(workspace, head, base_ref, base_sha)?;
        if after.commits_behind != 0 || after.commits_ahead == 0 {
            return Err(OrbitError::Execution(format!(
                "git_rebase: rebasing recovered HEAD '{recovered}' onto '{base_sha}' did not \
                 leave a candidate on that base"
            )));
        }
        let head_sha = commit_sha(workspace, head)?;
        certify_chase(host, input, context, checkpoint, recovered, &head_sha)?;
        return Ok(json!({
            "phase": "rebase",
            "decision": "chased_recovery",
            "head": head,
            "head_sha": head_sha,
            "head_sha_before": head_sha_before,
            "base": required_input_string(input, "base")?,
            "base_ref": base_ref,
            "base_sha": base_sha,
            "remote_sha_before": input_string_field(input, "remote_sha"),
            "rewritten": true,
        }));
    }

    let conflicting_paths = if chased.timed_out {
        Vec::new()
    } else {
        unmerged_paths(workspace)?
    };
    if rebase_in_progress(workspace)? {
        abort_owned_rebase(workspace)?;
    }
    if commit_sha(workspace, head)? != recovered {
        return Err(OrbitError::Execution(format!(
            "git_rebase: aborting the rebase of recovered HEAD '{recovered}' onto '{base_sha}' \
             did not restore it"
        )));
    }
    if chased.timed_out {
        let timeout = git_timeout_error(
            workspace,
            &["rebase", base_sha],
            chased.timeout_ms,
            &chased.stderr,
        );
        return Err(timeout_recovery_error(
            chased.timeout_ms,
            format!(
                "{timeout}; the rebase of recovered HEAD '{recovered}' onto '{base_sha}' was aborted \
             and the recovered HEAD kept. This is timeout recovery, not a merge conflict."
            ),
        ));
    }
    if conflicting_paths.is_empty() {
        return Err(git_failure_error(
            workspace,
            &["rebase", base_sha],
            &chased.stderr,
        ));
    }

    // The resolution the recovery certified does not carry onto the advanced
    // base. Recovery is only admitted on a rebase of the prepared HEAD, so
    // redo it from there; the earlier resolution stays reachable by its SHA.
    discard_rewrite(workspace, head_sha_before, what)?;
    let (decision, rewritten, head_sha) =
        perform_rebase_onto_base(workspace, head, head_sha_before, base_ref, base_sha).map_err(
            |error| match error {
                OrbitError::RecoverableVcsConflict(mut conflict) => {
                    conflict.diagnostic = format!(
                        "{}; base '{base_ref}' advanced past the certified resolution \
                         '{recovered}' onto '{recovered_base}', which conflicts with it",
                        conflict.diagnostic
                    );
                    OrbitError::RecoverableVcsConflict(conflict)
                }
                other => other,
            },
        )?;
    Ok(json!({
        "phase": "rebase",
        "decision": decision,
        "head": head,
        "head_sha": head_sha,
        "head_sha_before": head_sha_before,
        "base": required_input_string(input, "base")?,
        "base_ref": base_ref,
        "base_sha": base_sha,
        "remote_sha_before": input_string_field(input, "remote_sha"),
        "rewritten": rewritten,
    }))
}

/// Certify a clean chase as `checkpoint`'s step's newest recovery: the
/// prepared rewrite, landed on the prepared base, with the host's own attempt.
fn certify_chase<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    context: &HandoffContext,
    checkpoint: &Value,
    recovered: &str,
    head_sha: &str,
) -> Result<(), OrbitError> {
    let workspace = context.workspace_path.as_path();
    let run_id = recovery_run_id(input, context);
    let step_id = required_input_string(checkpoint, "step_id")?;
    let head_sha_before = required_input_string(input, "head_sha")?;
    let base_sha = required_input_string(input, "base_sha")?;
    let workspace_path = workspace.to_string_lossy().into_owned();
    let dispatch = |error: DispatchError| OrbitError::Execution(error.to_string());
    let attempt = host
        .begin_rebase_recovery_attempt(
            run_id,
            step_id,
            &RebaseRecoveryAttemptScope {
                workspace_path: workspace_path.clone(),
                head_sha_before: head_sha_before.to_string(),
                target_base_sha: base_sha.to_string(),
            },
        )
        .map_err(dispatch)?;
    host.checkpoint_rebase_recovery(
        run_id,
        step_id,
        &json!({
            "run_id": run_id,
            "step_id": step_id,
            "task_ids": checkpoint["task_ids"],
            "workspace_path": workspace_path,
            "head": input["head"],
            "head_sha_before": head_sha_before,
            "original_base_sha": original_base_sha(workspace, head_sha_before, base_sha)?,
            "base_ref": input["base_ref"],
            "target_base_sha": base_sha,
            "base_sha": base_sha,
            "remote_sha_before": input.get("remote_sha").cloned().unwrap_or(Value::Null),
            "head_sha": head_sha,
            "chased_from": recovered,
            "companion_paths": [],
            "rewritten": true,
            "recovery_attempt": attempt,
        }),
    )
    .map_err(dispatch)
}
