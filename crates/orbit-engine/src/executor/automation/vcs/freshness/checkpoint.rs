//! Host-certified rebase recovery checkpoint lookup.
use super::super::super::input::required_input_string;
use super::super::git::git_command_success;
use super::super::handoff::HandoffContext;
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use serde_json::Value;
use std::path::Path;

/// Outcome of scanning a run's `rebase_recovery_checkpoints` for `head_sha`.
pub(super) enum RecoveryCheckpointLookup {
    /// A candidate matched by shape and carries the host's certificate; every
    /// remaining integrity check (source consistency, ancestry, retry
    /// lineage) also passed.
    Certified(Value),
    /// A candidate matched by shape (head/workspace/step/rewritten) but
    /// carries no host certificate — a pre-authority-boundary checkpoint and
    /// a forged one look identical here, and neither is authority. Distinct
    /// from [`Self::Absent`] so a caller that can safely redo the underlying
    /// work (see `rebase_pr_branch_inner`) knows a redo is warranted, while a
    /// caller with no such fallback keeps refusing outright.
    Uncertified,
    /// Nothing in the run's checkpoints matches this HEAD at all.
    Absent,
}

/// Read only host-written provenance, authenticating the original durable run
/// when a resume carries a copy. Advisory activity outputs never authorize HEAD.
///
/// The run store these entries come from is writable by managed leaves, so a
/// matching entry is a candidate, not authority. Every candidate must also
/// carry the host's certificate from [`RuntimeHost::verify_rebase_recovery`].
/// Whether a checkpoint's recorded workspace directory is `workspace`.
///
/// The host records the checkout it certified at its resolved path, while the
/// workspace of a later retry is whatever spelling its input carried. When the
/// worktree root is reached through a symlinked ancestor (macOS `/tmp` and
/// `/var`, a symlinked home) those differ in spelling only, and the checkpoint
/// of the very same directory must not be discarded as foreign. Two spellings
/// match when they name one existing directory; anything unresolvable does
/// not match, so a checkpoint for another checkout is still refused.
pub(in crate::executor::automation::vcs) fn recorded_workspace_matches(
    recorded: Option<&str>,
    workspace: &Path,
) -> bool {
    let Some(recorded) = recorded else {
        return false;
    };
    let recorded = Path::new(recorded);
    if recorded == workspace {
        return true;
    }
    matches!(
        (recorded.canonicalize(), workspace.canonicalize()),
        (Ok(recorded), Ok(workspace)) if recorded == workspace
    )
}

pub(super) fn recovery_checkpoint_lookup<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    workspace: &Path,
    head_sha: &str,
) -> Result<RecoveryCheckpointLookup, OrbitError> {
    let Some(state) = host.read_run_state(run_id)? else {
        return Ok(RecoveryCheckpointLookup::Absent);
    };
    let mut saw_uncertified_match = false;
    for (step_id, checkpoint) in &state.rebase_recovery_checkpoints {
        if !matches!(
            step_id.as_str(),
            "sync_base" | "complete_pr" | "complete_reviewed_pr"
        ) || checkpoint.get("head_sha").and_then(Value::as_str) != Some(head_sha)
            || !recorded_workspace_matches(
                checkpoint.get("workspace_path").and_then(Value::as_str),
                workspace,
            )
            || checkpoint.get("step_id").and_then(Value::as_str) != Some(step_id)
            || checkpoint.get("rewritten").and_then(Value::as_bool) != Some(true)
        {
            continue;
        }
        let source_run_id = required_input_string(checkpoint, "run_id")?;
        if !host.verify_rebase_recovery(source_run_id, step_id, checkpoint)? {
            // No certificate: this candidate is not usable evidence, whether
            // because it predates the authority boundary or was never the
            // host's own record. Note it and keep scanning rather than
            // trusting it.
            saw_uncertified_match = true;
            continue;
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
            super::super::resume::ensure_retry_descends_from(
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
        return Ok(RecoveryCheckpointLookup::Certified(checkpoint.clone()));
    }
    Ok(if saw_uncertified_match {
        RecoveryCheckpointLookup::Uncertified
    } else {
        RecoveryCheckpointLookup::Absent
    })
}

/// The certified recovery checkpoint for `head_sha`, or `None` if there is
/// none — whether because nothing matches at all or because the only match
/// carries no host certificate. Both are equally untrusted for a caller that
/// has no redo fallback of its own (see `resume.rs`'s identity checks); only
/// [`super::rebase_recovery::recovered_rewrite`] needs the finer distinction, via
/// [`recovery_checkpoint_lookup`] directly.
pub(in crate::executor::automation::vcs) fn recovered_head_checkpoint<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    workspace: &Path,
    head_sha: &str,
) -> Result<Option<Value>, OrbitError> {
    Ok(
        match recovery_checkpoint_lookup(host, run_id, workspace, head_sha)? {
            RecoveryCheckpointLookup::Certified(checkpoint) => Some(checkpoint),
            RecoveryCheckpointLookup::Uncertified | RecoveryCheckpointLookup::Absent => None,
        },
    )
}

pub(in crate::executor::automation::vcs) fn recovery_run_id<'a>(
    input: &'a Value,
    context: &'a HandoffContext,
) -> &'a str {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or(&context.batch_id)
}
