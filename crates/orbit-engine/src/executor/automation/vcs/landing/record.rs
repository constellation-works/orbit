//! Recording landing progress, completion and stops.

use orbit_common::OrbitError;
use orbit_types::workflow::DeliveryRefusal;
use orbit_types::workflow::handoff::{
    HandoffCandidate, LandingWaitGate, OwnerLandingRefusal, OwnerLandingRefusalKind,
};
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, HandoffLandingUpdate, RuntimeHost};

use super::super::completion_hold::{
    ChangedPaths, evaluate_completion_hold, record_landing_completion_hold,
};
use super::super::pr::diagnostics;
use super::super::pr::{PrMergeState, PullRequestStatus};
use super::run::LandingPass;

pub(super) fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    step: HandoffLandingStep,
    observed: Option<HandoffCandidate>,
    evidence: &Value,
) -> Result<(), OrbitError> {
    host.record_handoff_landing(&HandoffLandingUpdate {
        handoff_id: context.handoff_id.clone(),
        step: step.clone(),
        observed,
        evidence: evidence.to_string(),
    })?;
    match step {
        HandoffLandingStep::PublishIntent { .. } => {
            context.wrote.set(true);
            context.intent_open.set(true);
        }
        HandoffLandingStep::ResolveIntent { .. } => context.intent_open.set(false),
        HandoffLandingStep::Complete
        | HandoffLandingStep::Stop { .. }
        | HandoffLandingStep::StopForRequeue => {
            context.settled.set(true);
        }
    }
    Ok(())
}

pub(super) fn complete<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observed: HandoffCandidate,
    evidence: &Value,
) -> Result<Value, OrbitError> {
    record(
        host,
        context,
        HandoffLandingStep::Complete,
        Some(observed),
        evidence,
    )?;
    Ok(json!({
        "phase": "landed",
        "handoff_id": context.handoff_id,
        "task_id": context.task_id,
        "evidence": evidence,
    }))
}

/// Keep diagnostics in the existing opaque evidence string. The enclosing
/// durable attempt supplies its attempt number/run; `observed` stays empty.
pub(super) fn stop_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    repairable: bool,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    refuse_pr(
        host,
        context,
        observation,
        HandoffLandingStep::Stop { repairable },
        reason,
    )
}

/// Record the same landing stop with a deliberate held run outcome.
pub(super) fn hold_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    repairable: bool,
    refusal: DeliveryRefusal,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    let stopped = record_refusal(
        host,
        context,
        observation,
        HandoffLandingStep::Stop { repairable },
        reason,
        (refusal == DeliveryRefusal::BaseChecksPending)
            .then_some(LandingWaitGate::BaseChecksPending),
    )?;
    Ok(match stopped {
        OrbitError::Execution(message) => OrbitError::DeliveryRefused { refusal, message },
        other => other,
    })
}

/// [`stop_pr`] recorded as `step`, a [`HandoffLandingStep::Stop`] or
/// [`HandoffLandingStep::StopForRequeue`].
pub(super) fn refuse_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    step: HandoffLandingStep,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_refusal(host, context, observation, step, reason, None)
}

pub(super) fn stop_waiting_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_refusal(
        host,
        context,
        observation,
        HandoffLandingStep::Stop { repairable: false },
        reason,
        Some(LandingWaitGate::RequiredChecksPending),
    )?;
    Ok(OrbitError::DeliveryRefused {
        refusal: DeliveryRefusal::RequirementsUnsatisfied,
        message: format!("handoff_land: {reason}"),
    })
}

fn record_refusal<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    step: HandoffLandingStep,
    reason: &str,
    waiting_gate: Option<LandingWaitGate>,
) -> Result<OrbitError, OrbitError> {
    let requeue = matches!(step, HandoffLandingStep::StopForRequeue);
    let repairable = requeue || matches!(step, HandoffLandingStep::Stop { repairable: true });
    let refusal = OwnerLandingRefusal {
        schema_version: 1,
        kind: OwnerLandingRefusalKind::OwnerLandingRefusal,
        reason: diagnostics::reason(reason),
        repairable,
        waiting_gate,
        accepted: json!({
            "task":diagnostics::text(&context.task_id),
            "handoff":diagnostics::text(&context.handoff_id),
            "repository":diagnostics::text(&context.candidate.repository),
            "base":diagnostics::text(&context.candidate.base.commit),
            "candidate":diagnostics::text(&context.candidate.candidate.commit),
            "source_branch":diagnostics::text(&context.candidate.source_branch),
            "base_branch":diagnostics::text(&context.candidate.base_branch),
            "landing_branch":diagnostics::text(&context.candidate.landing_branch),
        }),
        owner_owed: diagnostics::collection(&context.owed_hosted_checks, |name| {
            diagnostics::text(name)
        }),
        observation: observation.clone(),
        workflow_prerequisites: json!({"state":"not_collected"}),
        emission_proof: json!({"state":"not_collected"}),
        requeue: requeue.then_some(true),
    };
    let evidence = serde_json::to_value(&refusal).map_err(|error| {
        OrbitError::Execution(format!("handoff_land: encode landing refusal: {error}"))
    })?;
    record(host, context, step, None, &evidence)?;
    let message = format!("handoff_land: {}", refusal.reason.value);
    // A spent repair already ended the claim and returned the task to the
    // backlog (or blocked it). Hold the run so that designed return is not
    // counted as a landing failure. The one automatic repair still fails
    // the run, which is every non-requeue stop recorded here.
    if requeue {
        Ok(OrbitError::DeliveryRefused {
            refusal: DeliveryRefusal::LandingRepairRequeue,
            message,
        })
    } else {
        Ok(OrbitError::Execution(message))
    }
}

/// Record a durable stop and return the error the run fails with, so the
/// evidence an operator reads and the failure they see say the same thing.
pub(super) fn stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_stop(host, context, reason, false)
}

/// [`stop`] for a candidate that conflicts with, or is stale against, the base
/// it lands on. The owner store hands it to one automatic repair.
pub(super) fn stop_for_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    record_stop(host, context, reason, true)
}

pub(super) fn record_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    reason: &str,
    repairable: bool,
) -> Result<OrbitError, OrbitError> {
    record(
        host,
        context,
        HandoffLandingStep::Stop { repairable },
        None,
        &Value::String(reason.to_string()),
    )?;
    Ok(OrbitError::Execution(format!("handoff_land: {reason}")))
}

/// [ORB-23365] The `completion.hold` gate of an owner landing under
/// `workflow.distributed_completion = "done"`: when a rule matches the
/// accepted candidate's diff or its task, record the same
/// `completion_held` hold `pr_complete` records and stop the attempt
/// before anything merges or lands, leaving the task in review for a
/// person to approve. Returns the error the run fails with when held,
/// `None` when the landing may proceed. Callers that first observe the
/// candidate already merged or contained settle that fact instead and
/// never reach this gate: a merge that already happened cannot be held.
pub(super) fn completion_hold_stop<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    changed: &ChangedPaths,
    run_id: &str,
) -> Result<Option<OrbitError>, OrbitError> {
    if !context.owner_distributed_completion_done {
        return Ok(None);
    }
    let rules = host.completion_hold_rules();
    if rules.is_empty() {
        return Ok(None);
    }
    let task = host.get_task(&context.task_id)?;
    let Some(hold) =
        evaluate_completion_hold(&rules, std::slice::from_ref(&task), changed, run_id)?
    else {
        return Ok(None);
    };
    record_landing_completion_hold(host, &context.handoff_id, &context.task_id, &hold)?;
    let reason = format!(
        "{}; the owner's landing stopped before merging anything, so the candidate \
         stays in review for a person to approve",
        hold.summary()
    );
    Ok(Some(stop(host, context, &reason)?))
}

/// Whether the provider reports the pull request's branch as behind its base,
/// which branch protection refuses until the branch is brought up to date.
pub(super) fn behind_base(status: &PullRequestStatus) -> bool {
    status
        .merge_state_status
        .as_deref()
        .is_some_and(|state| state.eq_ignore_ascii_case("BEHIND"))
}

pub(super) fn describe(state: &PrMergeState) -> String {
    match state {
        PrMergeState::Merged => "merged".to_string(),
        PrMergeState::Closed => "closed".to_string(),
        PrMergeState::Contradictory(reason) => format!("contradictory: {reason}"),
        PrMergeState::Blocked(reason) => format!("blocked: {reason}"),
        PrMergeState::Conflict => "conflict".to_string(),
        PrMergeState::Mergeable => "mergeable".to_string(),
        PrMergeState::Pending => "pending".to_string(),
    }
}
