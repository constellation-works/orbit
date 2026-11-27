//! The host commits a recovery's working-tree repair before retry or resume.
//!
//! The recovery sandbox keeps Git metadata read-only, so an agent repairing a
//! step that runs on the committed candidate (`validate`, branch preparation,
//! publication) can edit files but never commit them, and the retry would
//! meet the same head with a dirty tree. Once recovery admits the retry,
//! Orbit commits the repair itself, as the `commit` step does for the
//! implementer, and the retry runs on that head as its evidence.

use orbit_types::workflow::ReviewGateAdmissionOutput;

use crate::executor::automation::vcs::{
    RecoveryCommit, RecoveryCommitRefusal, RecoveryCommitRequest, commit_recovery_repair,
};

use super::super::final_recovery::RunWorktree;
use super::*;
use crate::executor::automation::vcs::git::{git_command_success, git_output, is_ancestor};

/// The final-recovery file repair the host may commit: the failed step, the
/// recovery activity, the plan's `commit` step and the checkout recovery
/// observed (root, head, branch) before it ran.
pub(in crate::activity_job::job_executor) struct FinalRecoveryRepair<'a> {
    pub(in crate::activity_job::job_executor) step: &'a JobV2Step,
    pub(in crate::activity_job::job_executor) activity: &'a ResolvedRecoveryActivity,
    pub(in crate::activity_job::job_executor) commit_step: &'a JobV2Step,
    pub(in crate::activity_job::job_executor) task_id: &'a str,
    pub(in crate::activity_job::job_executor) worktree: &'a RunWorktree,
    pub(in crate::activity_job::job_executor) source_before:
        Option<(&'a std::path::Path, &'a str, &'a str)>,
    pub(in crate::activity_job::job_executor) recovery_model: Option<&'a str>,
}

/// Commit what `recovery` left in the worktree when the failed step runs on
/// a committed candidate. Nothing happens before the candidate's commit or
/// for the `commit` step itself, which commit the repair themselves, nor for
/// agent steps, whose changes the commit step or review settlement own.
///
/// `review_follows` admits a repair of a reviewed head: an `--auto-fix` check
/// repair on a completion step whose repaired head goes to a re-review round
/// before anything publishes or merges it.
pub(super) fn commit_recovery_repair_for_retry(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    bound: &Value,
    review_follows: bool,
    recovery_model: Option<&str>,
) -> Result<(), RecoveryCommitRefusal> {
    if !matches!(deterministic_action(step), Some(action) if action != "git_commit") {
        return Ok(());
    }
    commit_recovery_repair_after_commit(step, ctx, recovery, bound, review_follows, recovery_model)
}

/// Shared adoption boundary for step retry and final-recovery resume. The
/// final hook may fail in an agent phase, but must still judge file repairs
/// against the committed candidate's no-diff and review checkpoints.
pub(in crate::activity_job::job_executor) fn commit_recovery_repair_after_commit(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    bound: &Value,
    review_follows: bool,
    recovery_model: Option<&str>,
) -> Result<(), RecoveryCommitRefusal> {
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
    let reviewed_heads = blocking_reviewed_heads(&step.id, &outputs, review_follows);
    let task_ids = match (bound.get("task_id"), bound.get("task_ids")) {
        (Some(Value::String(id)), _) => vec![id.clone()],
        (_, Some(Value::Array(ids))) => ids
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let repair_paths = bound
        .get("ci_repair_paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    // The commit type is the tasks' own; an unreadable task counts as a chore.
    let commit_type = orbit_types::workflow::CommitType::for_bundle(
        task_ids
            .iter()
            .filter_map(|id| ctx.host.get_task(id).ok())
            .map(|task| task.task_type),
    );
    let request = RecoveryCommitRequest {
        workspace_path: std::path::Path::new(workspace_path),
        run_id: &ctx.run_id,
        failed_step_id: &step.id,
        recovery_activity: &recovery.name,
        task_ids: &task_ids,
        commit_type,
        recovery_model: recovery_model.map(str::to_string),
        repair_paths: bound
            .get("ci_repair_paths")
            .and_then(Value::as_array)
            .map(|_| &repair_paths[..]),
        no_diff_route: checkpoint
            .get("skipped_no_diff_expected")
            .and_then(Value::as_bool)
            == Some(true),
        reviewed_heads: &reviewed_heads,
    };
    match commit_recovery_repair(&ctx.host.process_env(), &request) {
        Ok(RecoveryCommit::Clean) => Ok(()),
        Ok(RecoveryCommit::Committed { commit_sha, paths }) => {
            tracing::info!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                commit_sha = %commit_sha,
                paths = ?paths,
                "committed the recovery repair before retry or resume"
            );
            if let Some(evidence) = bound.get("ci_repair_evidence") {
                super::check_scope::record(ctx, &task_ids, evidence, &commit_sha, &paths)?;
            }
            Ok(())
        }
        Err(refusal) => {
            tracing::warn!(
                target: "orbit.engine.job_executor",
                run_id = %ctx.run_id,
                failed_step_id = %step.id,
                recovery_activity = %recovery.name,
                reason = refusal.code(),
                "refused to commit the recovery repair; no retry or resume"
            );
            Err(refusal)
        }
    }
}

/// Whether `root` is inside a rebase stopped on unmerged entries, with HEAD
/// detached as Git leaves it.
fn stopped_rebase(
    process_env: &orbit_common::Env,
    root: &std::path::Path,
) -> Result<bool, orbit_common::OrbitError> {
    if git_command_success(process_env, root, &["symbolic-ref", "--quiet", "HEAD"])? {
        return Ok(false);
    }
    let mut in_progress = false;
    for backend in ["rebase-merge", "rebase-apply"] {
        let path = std::path::PathBuf::from(git_output(
            process_env,
            root,
            &["rev-parse", "--git-path", backend],
        )?);
        if root.join(path).is_dir() {
            in_progress = true;
        }
    }
    Ok(in_progress && !git_output(process_env, root, &["ls-files", "--unmerged"])?.is_empty())
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

/// Adopt only file edits to the exact checkout, branch and candidate the
/// engine handed to final recovery. The ordinary commit boundary still owns
/// repairs before commit; committed candidates use the existing host repair
/// guards before any index mutation, then the recorded head rewinds delivery.
pub(in crate::activity_job::job_executor) fn commit_final_recovery_repair(
    ctx: &ExecCtx<'_>,
    repair: FinalRecoveryRepair<'_>,
) -> Result<(), RecoveryCommitRefusal> {
    let FinalRecoveryRepair {
        step,
        activity,
        commit_step,
        task_id,
        worktree,
        source_before,
        recovery_model,
    } = repair;
    let pipeline = ctx.pipeline_snapshot();
    let Some(checkpoint) = pipeline
        .get(&commit_step.id)
        .map(unwrap_step_output)
        .filter(|output| output.get("phase").and_then(Value::as_str) == Some("commit"))
    else {
        return Ok(());
    };
    let refused = |detail: &str| RecoveryCommitRefusal::Uncommittable(detail.into());
    let root = std::path::Path::new(&worktree.workspace_path);
    let output = |args: &[&str]| {
        git_output(&ctx.host.process_env(), root, args)
            .map_err(|error| RecoveryCommitRefusal::Uncommittable(error.to_string()))
    };
    if output(&["status", "--porcelain"])?.is_empty() {
        return Ok(());
    }
    // A rebase stopped on conflicts (detached HEAD, unmerged entries) is not
    // a repair this host commits: the resumed rebasing step reports the same
    // stop, and conflict recovery's authenticated host continuation adopts
    // the repaired files and continues it [F2026-11-277]. Every other
    // detached or moved checkout still needs its symbolic branch below.
    if stopped_rebase(&ctx.host.process_env(), root).map_err(|error| refused(&error.to_string()))? {
        return Ok(());
    }
    let (root_before, head_before, branch_before) = source_before
        .ok_or_else(|| refused("final recovery has no observed checkout, candidate or branch"))?;
    let canonical = root
        .canonicalize()
        .map_err(|error| refused(&error.to_string()))?;
    let git_root = std::path::PathBuf::from(output(&["rev-parse", "--show-toplevel"])?);
    if root_before != canonical.as_path()
        || git_root != canonical
        || checkpoint.get("task_id").and_then(Value::as_str) != Some(task_id)
        || head_before != output(&["rev-parse", "--verify", "HEAD^{commit}"])?.as_str()
        || branch_before != output(&["symbolic-ref", "--quiet", "HEAD"])?.as_str()
    {
        return Err(refused(
            "final recovery file repair no longer matches its assigned checkout, task, branch or candidate",
        ));
    }
    let base = worktree
        .base_sha
        .as_deref()
        .ok_or_else(|| refused("final recovery has no pinned base for its file repair"))?;
    if !is_ancestor(&ctx.host.process_env(), root, base, head_before)
        .map_err(|error| refused(&error.to_string()))?
    {
        return Err(refused(
            "final recovery candidate does not descend from its pinned base",
        ));
    }
    commit_recovery_repair_after_commit(
        step,
        ctx,
        activity,
        &serde_json::json!({
            "task_id": task_id, "workspace_path": worktree.workspace_path,
        }),
        false,
        recovery_model,
    )
}

/// Review pins that prohibit a recovery commit on `HEAD` unless a later
/// review follows in the plan. Unsettled `BeforeLanding` admissions on
/// `landing_review` itself are excluded when recovering mid-review before
/// rewinding past `commit` -> `landing_review`.
fn blocking_reviewed_heads(step_id: &str, outputs: &[Value], review_follows: bool) -> Vec<String> {
    let is_current_landing_review = step_id == "landing_review";
    outputs
        .iter()
        .flat_map(|output| {
            let admitted_head =
                ReviewGateAdmissionOutput::from_value(output)
                    .ok()
                    .and_then(|admission| {
                        admission
                            .admitted()
                            .filter(|admitted| {
                                !admitted.attempt_id.is_empty()
                                    && !(is_current_landing_review
                                        && admitted.timing
                                            == orbit_types::workflow::ReviewTiming::BeforeLanding)
                            })
                            .map(|admitted| admitted.head_sha.clone())
                    });
            [
                admitted_head,
                output
                    .get("reviewed_head_sha")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ]
        })
        .flatten()
        .filter(|head| !head.is_empty() && !review_follows)
        .collect()
}
