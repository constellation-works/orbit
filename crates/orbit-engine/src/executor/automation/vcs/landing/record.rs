//! Recording landing progress, completion and stops.

use orbit_common::OrbitError;
use orbit_types::workflow::handoff::HandoffCandidate;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, HandoffLandingUpdate, RuntimeHost};

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

/// [`stop_pr`] recorded as `step`, a [`HandoffLandingStep::Stop`] or
/// [`HandoffLandingStep::StopForRequeue`].
pub(super) fn refuse_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    observation: &Value,
    step: HandoffLandingStep,
    reason: &str,
) -> Result<OrbitError, OrbitError> {
    let requeue = matches!(step, HandoffLandingStep::StopForRequeue);
    let repairable = requeue || matches!(step, HandoffLandingStep::Stop { repairable: true });
    let reason = diagnostics::reason(reason);
    let mut evidence = json!({
        "schema_version":1, "kind":"owner_landing_refusal",
        "reason":reason, "repairable":repairable,
        "accepted":{
            "task":diagnostics::text(&context.task_id),
            "handoff":diagnostics::text(&context.handoff_id),
            "repository":diagnostics::text(&context.candidate.repository),
            "base":diagnostics::text(&context.candidate.base.commit),
            "candidate":diagnostics::text(&context.candidate.candidate.commit),
            "source_branch":diagnostics::text(&context.candidate.source_branch),
            "base_branch":diagnostics::text(&context.candidate.base_branch),
            "landing_branch":diagnostics::text(&context.candidate.landing_branch),
        },
        "owner_owed": diagnostics::collection(&context.owed_hosted_checks, |name| diagnostics::text(name)),
        "observation":observation,
        "workflow_prerequisites":{"state":"not_collected"},
        "emission_proof":{"state":"not_collected"},
    });
    if requeue {
        evidence["requeue"] = json!(true);
    }
    record(host, context, step, None, &evidence)?;
    Ok(OrbitError::Execution(format!(
        "handoff_land: {}",
        reason["value"].as_str().unwrap_or_default()
    )))
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
