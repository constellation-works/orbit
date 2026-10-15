//! Trusted operator admission for one held-candidate recovery.
mod continuation;
mod observe;
mod submit;
pub(crate) use continuation::prepare;

use crate::OrbitRuntime;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::required_string;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{HELD_RECOVERY_ADMITTED, HeldRecoveryAdmission};
use serde_json::{Value, json};

pub(crate) const OPERATION: &str = "orbit.task.review_recover";

pub(super) fn refused(message: impl Into<String>) -> OrbitError {
    OrbitError::CapabilityDenied(format!("held recovery refused: {}", message.into()))
}

pub(super) fn reservations(
    runtime: &OrbitRuntime,
    task: &str,
) -> Result<Vec<HeldRecoveryAdmission>, OrbitError> {
    runtime
        .get_task_history(task)?
        .into_iter()
        .filter(|entry| entry.event == HELD_RECOVERY_ADMITTED)
        .map(|entry| {
            if entry.by != "system" {
                return Err(refused("untrusted reservation"));
            }
            serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                .map_err(|e| refused(format!("unreadable reservation: {e}")))
        })
        .collect()
}

/// Every action, including inspection, requires independently authenticated
/// operator authority. Caller-shaped JSON and managed envelopes confer none.
pub(crate) fn recover_review(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: &Value,
) -> Result<Value, OrbitError> {
    if orbit_common::governance::authorization::agent_context_declared()
        || runtime.worker_invocation().is_some()
    {
        return Err(refused("managed executions cannot admit recovery"));
    }
    let authority = runtime.admit_operator_operation(OPERATION, session)?;
    let id = required_string(input, &["id"], "id")?;
    let action = required_string(input, &["action"], "action")?;
    match action.as_str() {
        "inspect" => {
            let observation = observe::observe(runtime, &id, input)?;
            Ok(
                json!({"id":id,"binding":observation.binding,"diagnostics":observation.diagnostics,
                "reservations":reservations(runtime, &id)?,"eligible":true}),
            )
        }
        "status" => Ok(
            json!({"id":id,"reservations":reservations(runtime, &id)?,"continuations":continuations(runtime, &id)?}),
        ),
        "submit" => {
            runtime.ensure_coordination_task_write_permitted()?;
            let actor = authority
                .remote_caller_machine_id
                .clone()
                .map_or_else(|| runtime.actor().resolve_write_label(None, None), Ok)?;
            // Share the claim/reservation admission section. No grant can race
            // between observing the source and publishing the durable decision.
            runtime.admission_boundary()?.with_admission(|| {
                submit::submit(
                    runtime,
                    &id,
                    input,
                    &actor,
                    &authority.provenance.to_string(),
                )
            })
        }
        _ => Err(OrbitError::InvalidInput(
            "action must be inspect, submit or status".into(),
        )),
    }
}

/// Authenticate a continuing leaf against the owner's immutable pull receipt.
/// A generic job copying its JSON has neither a worker binding nor a receipt.
pub(crate) fn claimed_admission(
    runtime: &OrbitRuntime,
    run_id: &str,
) -> Result<Option<HeldRecoveryAdmission>, OrbitError> {
    if runtime.worker_invocation().is_none() {
        return Ok(None);
    }
    let leaf = runtime.live_claimed_leaf(run_id)?;
    let candidate = leaf
        .admission
        .receipt
        .as_ref()
        .and_then(|r| r.task.as_ref())
        .and_then(|t| t.resume_candidate.as_ref());
    let Some(admission) = candidate.and_then(|c| c.held_recovery.as_ref()) else {
        return Ok(None);
    };
    let binding = &admission.binding;
    let task = runtime.get_task(&leaf.claim.task_id)?;
    let run = runtime
        .get_job_run_backend(run_id)?
        .ok_or_else(|| refused("continuing run missing"))?;
    if binding.workspace_id != leaf.binding.owner_workspace_id
        || binding.task_id != leaf.claim.task_id
        || binding.proposed != orbit_types::workflow::HeldRecoveryMeaning::from_task(&task)
        || binding.proposed_spec_digest != task.spec_digest()
        || match binding.disposition {
            orbit_types::workflow::HeldRecoveryDisposition::SourceRepair => {
                run.job_id != "task_claimed_pr_pipeline"
            }
            orbit_types::workflow::HeldRecoveryDisposition::AdjudicateEvidence => {
                run.job_id != "task_claimed_adjudication_pipeline"
            }
        }
        || run.input.as_ref().is_none_or(|i| {
            i.get("resume_candidate") != serde_json::to_value(candidate).ok().as_ref()
        })
        || !leaf.admission.request.ship.before_pr && !leaf.admission.request.ship.before_landing
    {
        return Err(refused(
            "claim, scope, task meaning, PR route or review contract changed",
        ));
    }
    Ok(Some(admission.clone()))
}

/// An adjudication can uphold, reject, or propose replacement proof. It
/// cannot itself report unavailable proof as passing or waive failed checks.
pub(crate) fn check_report(
    admission: &HeldRecoveryAdmission,
    report: &mut orbit_types::workflow::ReviewReport,
) -> Result<(), String> {
    use orbit_types::workflow::{
        HeldEvidenceDecision, HeldRecoveryDisposition, ReviewEvidenceHold, ReviewVerdict,
    };
    if admission.binding.disposition != HeldRecoveryDisposition::AdjudicateEvidence {
        return if report.adjudication.is_none() {
            Ok(())
        } else {
            Err("source repair cannot waive assertions through evidence adjudication".into())
        };
    }
    let hold: ReviewEvidenceHold = serde_json::from_value(admission.evidence["hold"].clone())
        .map_err(|_| "original hold missing")?;
    let meaningful = |reason: &str| reason.trim().chars().count() >= 20;
    match report.adjudication.as_ref() {
        Some(HeldEvidenceDecision::Uphold {reason}) if meaningful(reason)
            && report.verdict == ReviewVerdict::Incomplete && report.external_evidence == hold.requirements => Ok(()),
        Some(HeldEvidenceDecision::Reject {reason}) if meaningful(reason)
            && report.verdict == ReviewVerdict::Reject && report.external_evidence.is_empty() => Ok(()),
        Some(HeldEvidenceDecision::Replace {reason, requirements}) if meaningful(reason)
            && !requirements.is_empty() && *requirements != hold.requirements
            && requirements.iter().all(|new| hold.requirements.iter().all(|old| old.artifact != new.artifact && old.command != new.command))
            && super::evidence::canonical_requirements(requirements).is_some()
            && requirements.iter().all(|new| !admission.binding.review.required_validation_commands.as_ref().is_some_and(|commands| commands.contains(&new.command)))
            && report.external_evidence == *requirements
            && report.verdict == ReviewVerdict::Incomplete => Ok(()),
        _ => Err("evidence adjudication requires an explicit, meaningful uphold/reject/replacement decision; replacement proof remains incomplete until genuine matching evidence arrives".into()),
    }
}

/// Ordinary owner submissions cannot consume a claim-only recovery admission.
/// Explicitly discarding its candidate retires reuse through the existing flow.
pub(crate) fn pending(runtime: &OrbitRuntime, task: &str) -> Result<bool, OrbitError> {
    let history = runtime.get_task_history(task)?;
    let Some(admitted) = history
        .iter()
        .rev()
        .find(|e| e.event == HELD_RECOVERY_ADMITTED)
    else {
        return Ok(false);
    };
    Ok(!history.iter().any(|e| {
        e.at >= admitted.at
            && (e.event == orbit_types::workflow::HELD_RECOVERY_CLAIMED
                || e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT)
    }))
}

/// Resolve consumption from the immutable history and the owner claim journal.
/// Missing bind/settlement means an unknown outcome, never a second admission.
pub(super) fn continuations(runtime: &OrbitRuntime, task: &str) -> Result<Vec<Value>, OrbitError> {
    let claims = runtime.inspect_execution_claims()?;
    runtime
        .get_task_history(task)?
        .iter()
        .filter(|e| e.event == orbit_types::workflow::HELD_RECOVERY_CLAIMED)
        .map(|entry| {
            if entry.by != "system" {
                return Err(refused("untrusted continuation link"));
            }
            let mut link: Value =
                serde_json::from_str(entry.note.as_deref().unwrap_or_default())
                    .map_err(|e| refused(format!("unreadable continuation link: {e}")))?;
            let claim = claims.iter().find(|c| {
                Some(c.claim.claim_id.as_str()) == link["claim_id"].as_str()
                    && c.claim.task_id == task
            });
            link["claim"] = serde_json::to_value(claim).map_err(|e| refused(e.to_string()))?;
            Ok(link)
        })
        .collect()
}
