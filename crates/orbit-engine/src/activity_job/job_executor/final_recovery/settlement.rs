use orbit_types::workflow::{FinalRecoveryDecision, FinalRecoveryRepairCommit};

use crate::context::{FinalRecoveryApplication, FinalRecoveryApplied, TaskAutomationUpdate};
use crate::executor::automation::vcs::absorbed::AbsorbedCandidate;
use crate::executor::automation::vcs::git::{git_command_success, git_output};

use super::super::*;
use super::types;

/// Hand the host a `complete_no_diff` for a verified absorbed candidate's
/// covering commit, as the decision no agent needed to make.
pub(super) fn settle_absorbed(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    task_id: String,
    worktree: types::RunWorktree,
    candidate: &AbsorbedCandidate,
) -> types::FinalRecoveryVerdict {
    let application = FinalRecoveryApplication {
        task_id,
        failed_step_id: step.id.clone(),
        decision: FinalRecoveryDecision::CompleteNoDiff {
            evidence_commit: candidate.covering_commit.clone(),
            rationale: format!(
                "conflict recovery left the candidate on base {} with no commit of its own; \
                 {} on '{}' already carries its change. Verified by the engine; no final \
                 recovery agent was dispatched.",
                candidate.base_sha, candidate.covering_commit, candidate.base_ref
            ),
        },
        resume_step_index: None,
        preserved_steps: Vec::new(),
        repair_commit: None,
        resume_context: None,
        workspace_path: worktree.workspace_path.into(),
        completion_done: ctx.input.get("completion").and_then(Value::as_str) == Some("done"),
        infrastructure: None,
    };
    conclude(job, ctx, step, activity, &application, None, None)
}

pub(super) fn observe_repair_commit(
    workspace_path: &str,
    head_before: Option<&str>,
) -> Option<FinalRecoveryRepairCommit> {
    let workspace_path = std::path::Path::new(workspace_path).canonicalize().ok()?;
    let head_sha_before = head_before?.to_string();
    let head_sha = git_output(&workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"]).ok()?;
    if head_sha == head_sha_before
        || !git_command_success(
            &workspace_path,
            &["merge-base", "--is-ancestor", &head_sha_before, &head_sha],
        )
        .ok()?
    {
        return None;
    }
    Some(FinalRecoveryRepairCommit {
        workspace_path,
        head_sha_before,
        head_sha,
    })
}

/// Hand `application` to the host and turn its answer into the verdict.
pub(super) fn conclude(
    job: &JobV2,
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    application: &FinalRecoveryApplication,
    resume_index: Option<usize>,
    downgrade: Option<&str>,
) -> types::FinalRecoveryVerdict {
    let applied = ctx.host.apply_final_recovery(&ctx.run_id, application);
    let kind = Some(application.decision.kind());
    match (applied, resume_index) {
        (Ok(FinalRecoveryApplied::Resume), Some(index)) => {
            let Some(context) = application.resume_context.as_ref() else {
                return types::FinalRecoveryVerdict::Escalated;
            };
            let mut detail = format!(
                "rerunning from step `{}`: {}",
                job.steps[index].id, context.rationale
            );
            if let Some(downgrade) = downgrade {
                detail.push_str(&format!("; {downgrade}"));
                if let Err(error) = ctx.host.apply_task_automation_update(
                    &application.task_id,
                    TaskAutomationUpdate {
                        append_comments: vec![orbit_types::task::TaskComment {
                            at: chrono::Utc::now(),
                            by: "system".to_string(),
                            message: format!("Final recovery for run `{}`: {detail}.", ctx.run_id),
                            operation_id: None,
                        }],
                        ..Default::default()
                    },
                ) {
                    tracing::warn!(
                        target: "orbit.engine.job_executor",
                        run_id = %ctx.run_id,
                        error = %error,
                        "final recovery resume downgrade task comment not recorded"
                    );
                    detail.push_str(&format!("; task comment could not be recorded: {error}"));
                }
            }
            emit_final_recovery_event(ctx, step, activity, "resume", kind, Some(&detail));
            types::FinalRecoveryVerdict::Resume(index, Box::new(context.clone()))
        }
        (Ok(FinalRecoveryApplied::Settled { outcome }), _) => {
            emit_final_recovery_event(ctx, step, activity, "settled", kind, Some(&outcome));
            types::FinalRecoveryVerdict::Settled
        }
        (Ok(FinalRecoveryApplied::Escalated { outcome }), _) => {
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(&outcome));
            types::FinalRecoveryVerdict::Escalated
        }
        (Ok(FinalRecoveryApplied::Resume), None) => {
            let detail = "the host answered resume for a decision that names no step";
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(detail));
            types::FinalRecoveryVerdict::Escalated
        }
        (Err(error), _) => {
            let detail = format!("applying the decision failed: {error}");
            emit_final_recovery_event(ctx, step, activity, "escalated", kind, Some(&detail));
            types::FinalRecoveryVerdict::Escalated
        }
    }
}

pub(super) fn emit_final_recovery_event(
    ctx: &ExecCtx<'_>,
    step: &JobV2Step,
    activity: &ResolvedRecoveryActivity,
    outcome: &str,
    decision: Option<&str>,
    detail: Option<&str>,
) {
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::FinalRecoveryAttempted {
            step_id: step.id.clone(),
            final_recovery_activity: activity.name.clone(),
            outcome: outcome.to_string(),
            decision: decision.map(str::to_string),
            detail: detail.map(redacted_recovery_diagnostic),
        },
    );
}
