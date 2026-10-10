//! [ORB-14822] The host commits a step recovery's working-tree repair before
//! the post-recovery attempt.
//!
//! The recovery sandbox keeps Git metadata read-only, so an agent repairing a
//! step that runs on the committed candidate (`validate`, branch preparation,
//! publication) can edit files but never commit them, and the retry would
//! meet the same head with a dirty tree. Once recovery admits the retry,
//! Orbit commits the repair itself, as the `commit` step does for the
//! implementer, and the retry runs on that head as its evidence.

use crate::executor::automation::vcs::{
    RecoveryCommit, RecoveryCommitRefusal, RecoveryCommitRequest, commit_recovery_repair,
};

use super::*;

/// Commit what `recovery` left in the worktree when the failed step runs on
/// a committed candidate. Nothing happens before the candidate's commit or
/// for the `commit` step itself, which commit the repair themselves, nor for
/// agent steps, whose changes the commit step or review settlement own.
pub(super) fn commit_recovery_repair_for_retry(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    bound: &Value,
) -> Result<(), RecoveryCommitRefusal> {
    if !matches!(deterministic_action(step), Some(action) if action != "git_commit") {
        return Ok(());
    }
    let pipeline = ctx.pipeline_snapshot();
    let outputs = pipeline
        .values()
        .map(unwrap_step_output)
        .collect::<Vec<_>>();
    // Every `git_commit` outcome records `phase: commit`; without one the
    // candidate is not committed yet and the `commit` step commits it.
    let Some(checkpoint) = outputs
        .iter()
        .find(|output| output.get("phase").and_then(Value::as_str) == Some("commit"))
    else {
        return Ok(());
    };
    let Some(workspace_path) = bound.get("workspace_path").and_then(Value::as_str) else {
        return Ok(());
    };
    // A review admitted on or settling this head pins it. An older pin, from
    // before a final-recovery resume moved the candidate, does not.
    let reviewed_heads = outputs
        .iter()
        .flat_map(|output| {
            let admitted = output.get("applies").and_then(Value::as_bool) == Some(true)
                && output
                    .get("attempt_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty());
            [
                admitted.then(|| output.get("head_sha")).flatten(),
                output.get("reviewed_head_sha"),
            ]
        })
        .flatten()
        .filter_map(Value::as_str)
        .filter(|head| !head.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let task_ids = match (bound.get("task_id"), bound.get("task_ids")) {
        (Some(Value::String(id)), _) => vec![id.clone()],
        (_, Some(Value::Array(ids))) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let request = RecoveryCommitRequest {
        workspace_path: std::path::Path::new(workspace_path),
        run_id: &ctx.run_id,
        failed_step_id: &step.id,
        recovery_activity: &recovery.name,
        task_ids: &task_ids,
        no_diff_route: checkpoint
            .get("skipped_no_diff_expected")
            .and_then(Value::as_bool)
            == Some(true),
        reviewed_heads: &reviewed_heads,
    };
    match commit_recovery_repair(&request) {
        Ok(RecoveryCommit::Clean) => Ok(()),
        Ok(RecoveryCommit::Committed { commit_sha, paths }) => {
            tracing::info!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                commit_sha = %commit_sha,
                paths = ?paths,
                "committed the step recovery's repair before the post-recovery attempt"
            );
            Ok(())
        }
        Err(refusal) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                reason = refusal.code(),
                "refused to commit the step recovery's repair; no post-recovery attempt"
            );
            Err(refusal)
        }
    }
}

fn deterministic_action(step: &JobV2Step) -> Option<&str> {
    match &step.body {
        JobV2StepBody::Target(TargetStep {
            spec: ActivityV2Spec::Deterministic(spec),
            ..
        }) => Some(spec.action.as_str()),
        _ => None,
    }
}
