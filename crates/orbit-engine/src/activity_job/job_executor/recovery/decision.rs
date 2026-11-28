//! Durable recovery decisions and retry admission.
use super::invocation::{RecoveryDispatch, RecoveryDispatchError, dispatch_recovery};
use super::observation::RecoveryObservation;
use super::*;
use crate::context::{
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionSlot, StepRecoveryVerdict,
};
use orbit_types::workflow::AgentBlocker;
use orbit_types::workflow::PR_CONFLICT_RECOVERY_ACTIVITY;
use orbit_types::workflow::activity_job::StepRecoveryDecisionRecord;

/// What a recovery invocation admits for its failed step.
pub(super) enum RecoveryAdmission {
    /// Make the post-recovery attempt. Carries the dispatched recovery's
    /// resolved model, which authors the host's repair commit [ORB-16768].
    Retry(Option<String>),
    /// Return the original failure. When `no_edit_eligible` is true, an
    /// auto-fix check repair that left no edit and no scope record may still
    /// record the unchanged head for one rerun instead of holding immediately.
    Original { no_edit_eligible: bool },
    /// Fail the step as the blocker recovery declared [ORB-14268].
    Blocked(AgentBlocker),
    /// Hold the step when recovery dispatch met a retryable host condition.
    Held(DispatchError),
}

/// Deferred `StepRecoveryAttempted` event so `retry.rs` can settle check-repair
/// scope, commit and no-edit rerun admission before the audit event and
/// no-retry warning are emitted.
pub(super) struct PendingRecoveryAttempt {
    step_id: String,
    recovery_activity: String,
    recovery_succeeded: bool,
    failure_phase: Option<String>,
    error_message: Option<String>,
    output: Option<Value>,
    decision: Option<StepRecoveryDecisionRecord>,
    /// Display of the host guard that vetoed a post-recovery attempt the
    /// decision file had admitted. The same text prefixes the returned error.
    host_refusal: Option<String>,
}

impl PendingRecoveryAttempt {
    /// Record that a clean, admitted no-edit check repair authorized one
    /// post-recovery rerun while preserving the decision file's status/verdict.
    pub(super) fn admit_no_edit_retry(&mut self) {
        if let Some(decision) = &mut self.decision {
            decision.retry_admitted = true;
        }
    }

    /// Record that a host guard refused the post-recovery attempt while
    /// preserving the decision file's outcome. `refusal` is the guard's
    /// display, the same text the returned step error starts with.
    pub(super) fn refuse_retry(&mut self, refusal: impl std::fmt::Display) {
        if let Some(decision) = &mut self.decision {
            decision.retry_admitted = false;
        }
        self.host_refusal = Some(refusal.to_string());
    }

    pub(super) fn emit(self, ctx: &ExecCtx<'_>) {
        let retry_admitted = self.recovery_succeeded
            && self
                .decision
                .as_ref()
                .is_none_or(|decision| decision.retry_admitted);
        if let Some(decision) = self.decision.as_ref().filter(|_| !retry_admitted) {
            if let Some(refusal) = self.host_refusal.as_deref() {
                // The decision file asked for a retry. The host vetoed the
                // attempt afterwards, so the warning must name that veto
                // rather than the verified decision.
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %self.step_id,
                    decision_status = %decision.status,
                    decision_verdict = decision.verdict.as_deref(),
                    decision_detail = decision.detail.as_deref(),
                    retry_admitted,
                    host_refusal = refusal,
                    "host admission refused the post-recovery attempt; returning the original failure"
                );
            } else {
                tracing::warn!(
                    target: "orbit.engine.job_executor",
                    run_id = %ctx.run_id,
                    failed_step_id = %self.step_id,
                    decision_status = %decision.status,
                    decision_verdict = decision.verdict.as_deref(),
                    decision_detail = decision.detail.as_deref(),
                    "recovery decision admits no post-recovery attempt; returning the original failure"
                );
            }
        }
        emit_job_event_lossy(
            &ctx.audit,
            ctx.task_id(),
            V2AuditEventKind::StepRecoveryAttempted {
                step_id: self.step_id,
                recovery_activity: self.recovery_activity,
                recovery_succeeded: self.recovery_succeeded,
                failure_phase: self.failure_phase,
                error_message: self.error_message,
                output: self.output,
                decision: self.decision,
            },
        );
    }
}

pub(super) fn attempt_recovery_activity(
    step: &JobV2Step,
    ctx: &ExecCtx<'_>,
    recovery: &ResolvedRecoveryActivity,
    failure: &StepFailure,
    attempt: u32,
    max_attempts: u32,
) -> (RecoveryAdmission, Option<PendingRecoveryAttempt>) {
    // The conflict leaf resolves a stopped rebase; a declared-failed outcome
    // or any other error is never a rebase conflict for it to resolve.
    if recovery.name == PR_CONFLICT_RECOVERY_ACTIVITY
        && !matches!(
            failure,
            StepFailure::Error(DispatchError::RecoverableVcsConflict(_))
        )
    {
        return (
            RecoveryAdmission::Original {
                no_edit_eligible: false,
            },
            None,
        );
    }

    let result = match ctx.host.authorize_step_recovery(&ctx.run_id, &step.id) {
        Ok(StepRecoveryAdmission::Allowed | StepRecoveryAdmission::Reserved { .. }) => {
            dispatch_recovery(step, ctx, recovery, failure, attempt, max_attempts)
        }
        Ok(StepRecoveryAdmission::Denied { reason }) => Err(RecoveryDispatchError::Failed {
            phase: "authorization",
            message: reason,
        }),
        Err(error) => Err(RecoveryDispatchError::Failed {
            phase: "authorization",
            message: error.to_string(),
        }),
    };

    let mut held_dispatch: Option<DispatchError> = None;
    let (recovery_succeeded, failure_phase, error_message, output, read, recovery_model) =
        match result {
            Ok(RecoveryDispatch {
                dispatch,
                slot,
                observation,
                recovery_model,
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
                    recovery_model,
                )
            }
            Err(RecoveryDispatchError::Held(error)) => {
                let message = error.to_string();
                held_dispatch = Some(error);
                (
                    false,
                    Some("dispatch".to_string()),
                    Some(redacted_recovery_diagnostic(&message)),
                    None,
                    None,
                    None,
                )
            }
            Err(RecoveryDispatchError::Failed { phase, message }) => (
                false,
                Some(phase.to_string()),
                Some(redacted_recovery_diagnostic(&message)),
                None,
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
    let admission = match (held_dispatch, &decision, blocker) {
        (Some(error), _, _) => RecoveryAdmission::Held(error),
        (_, _, _) if !recovery_succeeded => RecoveryAdmission::Original {
            no_edit_eligible: false,
        },
        (_, _, Some(blocker)) => RecoveryAdmission::Blocked(blocker),
        (_, None, None) => RecoveryAdmission::Retry(recovery_model),
        (_, Some(decision), None) if decision.retry_admitted => {
            RecoveryAdmission::Retry(recovery_model)
        }
        (_, Some(decision), None) => RecoveryAdmission::Original {
            no_edit_eligible: decision.status == "absent"
                || (decision.status == "verified"
                    && decision.verdict.as_deref()
                        == Some(StepRecoveryVerdict::NotRecovered.as_str())),
        },
    };
    let pending = PendingRecoveryAttempt {
        step_id: step.id.clone(),
        recovery_activity: recovery.name.clone(),
        recovery_succeeded,
        failure_phase,
        error_message,
        output,
        decision,
        host_refusal: None,
    };
    (admission, Some(pending))
}

/// Read the invocation's durable decision through the host [ORB-14152].
///
/// A verified `retry` admits the post-recovery attempt. An absent file admits
/// it only when the engine observes a change since the failure [ORB-14268]:
/// otherwise the rerun meets the same cause (except for a clean, admitted
/// `--auto-fix` check repair whose no-edit rerun ledger write succeeds in
/// `retry.rs`, which also applies to a verified `not_recovered` verdict).
/// A verified `external_blocker` returns its blocker. Anything present but not
/// bound to this invocation, and any read failure, refuses the attempt: neither
/// is evidence that recovery repaired the step.
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
