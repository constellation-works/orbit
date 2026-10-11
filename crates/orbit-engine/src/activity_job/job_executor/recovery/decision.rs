//! Durable recovery decisions and retry admission.
use super::invocation::{RecoveryDispatch, dispatch_recovery};
use super::observation::RecoveryObservation;
use super::*;
use crate::context::{
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionSlot, StepRecoveryVerdict,
};
use orbit_types::workflow::AgentBlocker;
use orbit_types::workflow::activity_job::StepRecoveryDecisionRecord;

/// What a recovery invocation admits for its failed step.
pub(super) enum RecoveryAdmission {
    /// Make the post-recovery attempt.
    Retry,
    /// Return the original failure.
    Original,
    /// Fail the step as the blocker recovery declared [ORB-14268].
    Blocked(AgentBlocker),
}

pub(super) fn attempt_recovery_activity(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    failure: &StepFailure,
    attempt: u32,
    max_attempts: u32,
) -> RecoveryAdmission {
    // The conflict leaf resolves a stopped rebase; a declared-failed outcome
    // or any other error is never a rebase conflict for it to resolve.
    if recovery.name == PR_CONFLICT_RECOVERY_ACTIVITY
        && !matches!(
            failure,
            StepFailure::Error(DispatchError::RecoverableVcsConflict { .. })
        )
    {
        return RecoveryAdmission::Original;
    }

    let recovery_started = std::time::Instant::now();
    let result = match ctx.host.authorize_step_recovery(&ctx.run_id, &step.id) {
        Ok(StepRecoveryAdmission::Allowed | StepRecoveryAdmission::Reserved { .. }) => {
            let result = dispatch_recovery(step, ctx, recovery, failure, attempt, max_attempts);
            // Preparation failures spend the reserved episode too.
            if let Err(error) = ctx.host.settle_step_recovery(
                &ctx.run_id,
                &step.id,
                recovery_started.elapsed().as_secs(),
            ) {
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %step.id,
                    error = %error,
                    "step recovery settlement failed"
                );
            }
            result
        }
        Ok(StepRecoveryAdmission::Denied { reason }) => Err(("authorization", reason)),
        Err(error) => Err(("authorization", error.to_string())),
    };

    let (recovery_succeeded, failure_phase, error_message, output, read) = match result {
        Ok(RecoveryDispatch {
            dispatch,
            slot,
            observation,
        }) => {
            let error_message = (!dispatch.success).then(|| {
                redacted_recovery_diagnostic(dispatch.message.as_deref().unwrap_or(
                    "recovery activity returned an unsuccessful outcome without a diagnostic",
                ))
            });
            // Only a completed invocation's decision is read. Its response
            // body stays advisory and never reaches this gate.
            let decision = slot
                .as_ref()
                .filter(|_| dispatch.success)
                .map(|slot| read_recovery_decision(ctx, slot, observation.as_ref()));
            (
                dispatch.success,
                (!dispatch.success).then(|| "activity".to_string()),
                error_message,
                Some(bounded_recovery_input(
                    &ctx.run_id,
                    orbit_common::security::redaction::redact_all_json(dispatch.output),
                )),
                decision,
            )
        }
        Err((phase, message)) => (
            false,
            Some(phase.to_string()),
            Some(redacted_recovery_diagnostic(&message)),
            None,
            None,
        ),
    };
    let (decision, blocker) = match read {
        Some((decision, blocker)) => (Some(decision), blocker),
        None => (None, None),
    };
    // Without a slot (custom or conflict recovery, or a host without the
    // capability) a completed recovery keeps the legacy single re-attempt.
    let admission = match (&decision, blocker) {
        _ if !recovery_succeeded => RecoveryAdmission::Original,
        (_, Some(blocker)) => RecoveryAdmission::Blocked(blocker),
        (None, None) => RecoveryAdmission::Retry,
        (Some(decision), None) if decision.retry_admitted => RecoveryAdmission::Retry,
        (Some(_), None) => RecoveryAdmission::Original,
    };
    let retry_admitted = matches!(admission, RecoveryAdmission::Retry);
    if let Some(decision) = decision.as_ref().filter(|_| !retry_admitted) {
        tracing::warn!(
            target: "orbit.engine.job_executor",
            run_id = %ctx.run_id,
            failed_step_id = %step.id,
            decision_status = %decision.status,
            decision_verdict = decision.verdict.as_deref(),
            decision_detail = decision.detail.as_deref(),
            "recovery decision admits no post-recovery attempt; returning the original failure"
        );
    }
    emit_job_event_lossy(
        &ctx.audit,
        ctx.task_id(),
        V2AuditEventKind::StepRecoveryAttempted {
            step_id: step.id.clone(),
            recovery_activity: recovery.name.clone(),
            recovery_succeeded,
            failure_phase,
            error_message,
            output,
            decision,
        },
    );
    admission
}

/// Read the invocation's durable decision through the host [ORB-14152].
///
/// A verified `retry` admits the post-recovery attempt. An absent file admits
/// it only when the engine observes a change since the failure [ORB-14268]:
/// otherwise the rerun meets the same cause. A verified `external_blocker`
/// returns its blocker. Anything present but not bound to this invocation,
/// and any read failure, refuses the attempt: neither is evidence that
/// recovery repaired the step.
fn read_recovery_decision(
    ctx: &ExecCtx<'_>,
    slot: &StepRecoveryDecisionSlot,
    observation: Option<&RecoveryObservation>,
) -> (StepRecoveryDecisionRecord, Option<AgentBlocker>) {
    let record =
        |status: &str, verdict: Option<StepRecoveryVerdict>, retry, detail: Option<&str>| {
            StepRecoveryDecisionRecord {
                status: status.to_string(),
                verdict: verdict.map(|verdict| verdict.as_str().to_string()),
                retry_admitted: retry,
                detail: detail.map(redacted_recovery_diagnostic),
            }
        };
    match ctx.host.read_step_recovery_decision(slot) {
        Ok(StepRecoveryDecisionRead::Absent) => {
            let changes = observation.map(|observation| observation.changes(ctx));
            let detail = match changes.as_deref() {
                Some([]) => "no decision was written and no change to the worktree, base or \
                             validation environment was observed since the failure"
                    .to_string(),
                Some(changes) => format!(
                    "no decision was written; changed since the failure: {}",
                    changes.join(", ")
                ),
                None => "no decision was written and no change since the failure could be \
                         observed"
                    .to_string(),
            };
            let retry = changes.is_some_and(|changes| !changes.is_empty());
            (record("absent", None, retry, Some(&detail)), None)
        }
        Ok(StepRecoveryDecisionRead::Verified {
            verdict,
            reason,
            blocker,
        }) => {
            let detail = match &blocker {
                Some(blocker) => Some(format!(
                    "kind={}: {}",
                    blocker.kind,
                    reason.as_deref().unwrap_or(&blocker.evidence)
                )),
                None => reason,
            };
            (
                record(
                    "verified",
                    Some(verdict),
                    verdict == StepRecoveryVerdict::Retry,
                    detail.as_deref(),
                ),
                blocker.filter(|_| verdict == StepRecoveryVerdict::ExternalBlocker),
            )
        }
        Ok(StepRecoveryDecisionRead::Invalid { diagnostic }) => {
            (record("invalid", None, false, Some(&diagnostic)), None)
        }
        Err(error) => (
            record("unavailable", None, false, Some(&error.to_string())),
            None,
        ),
    }
}
