//! Recovery admission and the post-recovery retry loop.
use super::commit::commit_recovery_repair_for_retry;
use super::decision::{RecoveryAdmission, attempt_recovery_activity};
use super::input::bind_recovery_context;
use super::*;
use crate::executor::automation::vcs::RecoveryCommitRefusal;
use orbit_types::workflow::AgentBlocker;

/// Most `pr_conflict_recovery` rounds one failing step gets [ORB-14332].
///
/// A rebase stops once per conflicting commit it replays, and a delivery
/// candidate carries its implementation commit plus at most one reviewer
/// commit per review round (before-PR review and two completion re-reviews).
/// Each round resolves one stop; a retry that stops yet again past this
/// bound fails the step like any other failed post-recovery attempt.
const MAX_CONFLICT_RECOVERY_ROUNDS: u32 = 4;

/// The failure that sends a step into recovery.
///
/// [DANI-10438] A CLI agent that finishes and declares `status: "failed"` in
/// its envelope reaches the executor as `Ok(StepOutcome { success: false })`,
/// not as an `Err` — and that is the case a recovery leaf exists for (red
/// gate, transient tooling, flaky test). Recovery is keyed on this enum so both
/// shapes take the same path; only the returned-unrecovered value differs.
pub(in crate::activity_job::job_executor) enum StepFailure {
    Error(DispatchError),
    Outcome(StepOutcome),
}

impl StepFailure {
    /// The diagnostic handed to the recovery activity and kept on the terminal
    /// error [ORB-10449].
    pub(super) fn diagnostic(&self) -> String {
        match self {
            Self::Error(error) => error.to_string(),
            Self::Outcome(outcome) => outcome
                .message
                .clone()
                .unwrap_or_else(|| "step completed with success=false".to_string()),
        }
    }

    /// Hand the original failure back unchanged when recovery is absent,
    /// refused, or fails.
    fn into_result(self) -> Result<StepOutcome, DispatchError> {
        match self {
            Self::Error(error) => Err(error),
            Self::Outcome(outcome) => Ok(outcome),
        }
    }

    /// [ORB-14268] Fail the step as the blocker its recovery declared. The
    /// marker leads the message, so final recovery skips it and the failure
    /// handoff blocks the task with the kind, as for an implementer blocker
    /// (or holds it in the backlog, for an upgrade refusal).
    fn into_blocked(self, blocker: &AgentBlocker) -> Result<StepOutcome, DispatchError> {
        let message = format!(
            "{}\noriginal error before recovery: {}",
            blocker.step_failure_message(),
            self.diagnostic()
        );
        match self {
            Self::Error(_) => Err(DispatchError::JobExecution(message)),
            Self::Outcome(outcome) => Ok(StepOutcome {
                success: false,
                output: outcome.output,
                message: Some(message),
            }),
        }
    }

    /// [ORB-14822] Fail the step without its retry: the host refused to
    /// commit the repair recovery left, and nothing was committed.
    fn into_refused_repair(
        self,
        refusal: &RecoveryCommitRefusal,
    ) -> Result<StepOutcome, DispatchError> {
        let message = format!(
            "{refusal}; original error before recovery: {}",
            self.diagnostic()
        );
        match self {
            Self::Error(_) => Err(DispatchError::JobExecution(message)),
            Self::Outcome(outcome) => Ok(StepOutcome {
                success: false,
                output: outcome.output,
                message: Some(message),
            }),
        }
    }
}

pub(in crate::activity_job::job_executor) fn recover_or_return_original(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    failure: StepFailure,
    attempt: u32,
    max_attempts: u32,
) -> Result<StepOutcome, DispatchError> {
    // Signing in requires the operator; a repair agent cannot make this
    // provider usable, nor give its selected model capacity [ORB-14149] or
    // its account usage [ORB-14695], and the post-recovery attempt would
    // rerun the same model. Preserve the marker for claimed-leaf settlement.
    if orbit_types::workflow::is_provider_unavailable(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // [ORB-14266] Nor can it get past the provider's content policy: the
    // repair agent and the rerun would send the same task to the same filter.
    if orbit_types::workflow::is_provider_refusal(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // [ORB-13987] Nor can it install a tool required validation could not
    // find; repairing the candidate would only spend the recovery budget.
    if orbit_types::workflow::is_validation_environment_failure(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // [ORB-14260] Nor can it reach a claimed task's owner when the step's
    // own coordinator could not: its sandbox masks the same credentials.
    // Preserve the marker for claimed-leaf settlement.
    if orbit_types::workflow::is_owner_route_unavailable(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // [ORB-14258] Nor can it fix a required command the base itself fails;
    // the task is held until the command passes on a new base instead.
    if orbit_types::workflow::is_baseline_red_failure(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // [ORB-14269] Nor can it clear a blocker the implementer already declared.
    // Another agent would only spend the recovery budget the stop was meant
    // to avoid.
    if orbit_types::workflow::is_task_blocked_by_agent(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    // Nor can it run `orbit` while an upgrade refuses the step's agents; the
    // step runs again once the upgrade settles.
    if orbit_types::workflow::is_upgrade_pending(None, Some(&failure.diagnostic())) {
        return failure.into_result();
    }
    let Some(recovery) = recovery_activity_for_step(step, ctx) else {
        return failure.into_result();
    };

    // Each conflict recovery round resolves one stop of the rebase. A retry
    // that stops on a later commit of that rebase, or on a base that moved
    // again, is a new typed conflict for the next round, up to the bound. A
    // retry reporting the very conflict just recovered made no progress, so
    // another round would only repeat it.
    let original = failure.diagnostic();
    let mut failure = failure;
    let mut round = 1;
    loop {
        match attempt_recovery_activity(step, ctx, &recovery, &failure, attempt, max_attempts) {
            RecoveryAdmission::Retry => {}
            RecoveryAdmission::Original => return failure.into_result(),
            RecoveryAdmission::Blocked(blocker) => return failure.into_blocked(&blocker),
        }
        // [ORB-14822] The agent cannot commit its repair; the host does,
        // and the retry below runs on that commit.
        if recovery.name == STEP_FAILURE_RECOVERY_ACTIVITY {
            let mut bound = Value::Object(serde_json::Map::new());
            let committed = bind_recovery_context(step, ctx, &mut bound)
                .map_err(|error| RecoveryCommitRefusal::Uncommittable(error.to_string()))
                .and_then(|()| commit_recovery_repair_for_retry(step, ctx, &recovery, &bound));
            if let Err(refusal) = committed {
                return failure.into_refused_repair(&refusal);
            }
        }
        let recovered_conflict = (recovery.name == PR_CONFLICT_RECOVERY_ACTIVITY
            && round < MAX_CONFLICT_RECOVERY_ROUNDS)
            .then(|| failure.diagnostic());
        match post_recovery_attempt(step, ctx, &recovery, &original, recovered_conflict) {
            PostRecovery::ConflictAgain(conflict) => {
                failure = StepFailure::Error(conflict);
                round += 1;
            }
            PostRecovery::Settled(result) => return result,
        }
    }
}

/// How the post-recovery attempt of a failed step ended.
enum PostRecovery {
    /// Recovered, or failed for good with the original diagnostic attached.
    Settled(Result<StepOutcome, DispatchError>),
    /// The retry stopped on a different typed rebase conflict and a further
    /// conflict recovery round is admitted.
    ConflictAgain(DispatchError),
}

fn post_recovery_attempt(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    original: &str,
    recovered_conflict: Option<String>,
) -> PostRecovery {
    let reattempt = run_step_body(step, ctx);
    let (outcome, error_message) = match &reattempt {
        Ok(outcome) if outcome.success => ("success", None),
        Ok(outcome) => (
            "failed",
            Some(
                outcome
                    .message
                    .clone()
                    .unwrap_or_else(|| "step completed with success=false".to_string()),
            ),
        ),
        // [ORB-14668] The recovery worked: the base already held the
        // candidate, which final recovery settles.
        Err(error @ DispatchError::CandidateAbsorbed { .. }) => {
            ("absorbed", Some(error.to_string()))
        }
        Err(error) => ("error", Some(error.to_string())),
    };
    let error_message = error_message.map(|message| redacted_recovery_diagnostic(&message));
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepPostRecoveryAttempt {
            step_id: step.id.clone(),
            recovery_activity: recovery.name.clone(),
            outcome: outcome.to_string(),
            error_message: error_message.clone(),
            output: reattempt.as_ref().ok().map(|outcome| {
                bounded_recovery_input(
                    &ctx.run_id,
                    orbit_common::security::redaction::redact_all_json(outcome.output.clone()),
                )
            }),
        },
    );

    match reattempt {
        Ok(outcome) if outcome.success => PostRecovery::Settled(Ok(outcome)),
        Err(conflict @ DispatchError::RecoverableVcsConflict(_))
            if recovered_conflict
                .as_deref()
                .is_some_and(|recovered| recovered != conflict.to_string()) =>
        {
            PostRecovery::ConflictAgain(conflict)
        }
        Err(DispatchError::CandidateAbsorbed { candidate, .. }) => {
            PostRecovery::Settled(Err(DispatchError::CandidateAbsorbed {
                candidate,
                message: format!(
                    "job executor: post-recovery attempt {outcome}: {}; original error before recovery: {original}",
                    error_message.unwrap_or_else(|| "no diagnostic".to_string()),
                ),
            }))
        }
        Ok(_) | Err(_) => PostRecovery::Settled(Err(DispatchError::JobExecution(format!(
            "post-recovery attempt {outcome}: {}; original error before recovery: {original}",
            error_message.unwrap_or_else(|| "no diagnostic".to_string()),
        )))),
    }
}

fn recovery_activity_for_step(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
) -> Option<ResolvedRecoveryActivity> {
    match (
        step.recovery_activity.as_ref(),
        step.resolved_recovery_activity.as_ref(),
    ) {
        (Some(name), Some(activity)) => Some(ResolvedRecoveryActivity {
            name: name.clone(),
            spec: activity.spec.clone(),
        }),
        (Some(_), None) => None,
        _ => ctx.recovery_activity.clone(),
    }
}
