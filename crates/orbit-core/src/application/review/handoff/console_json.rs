//! JSON renderers for the owner operator console.

use orbit_store::contracts::{ClaimInspection, ClaimMutationResult, ExecutionClaimPhase};
use orbit_types::workflow::handoff::{
    AcceptedHandoff, LandingAttempt, LandingAttemptState, LandingStartRequest, LandingStartState,
};

use crate::application::task;

pub(super) fn settled_claim_summary_json(claim: &ClaimInspection) -> serde_json::Value {
    let location = &claim.claim.executed_on;
    serde_json::json!({
        "claim_id": claim.claim.claim_id,
        "task_id": claim.claim.task_id,
        "host": location.machine_name.as_deref().unwrap_or(&location.machine_id),
        "outcome": task::execution_claim_phase_label(claim.claim.phase),
        "settled_at": claim.updated_at,
    })
}

pub(super) fn mutation_json(handoff_id: &str, result: &ClaimMutationResult) -> serde_json::Value {
    serde_json::json!({
        "handoff_id": handoff_id,
        "claim_id": result.claim_id,
        "phase": task::execution_claim_phase_label(result.phase),
        "task_status": result.status.to_string(),
    })
}

pub(super) fn phase_from_label(label: &str) -> Option<ExecutionClaimPhase> {
    [
        ExecutionClaimPhase::Claimed,
        ExecutionClaimPhase::Running,
        ExecutionClaimPhase::HandedOff,
        ExecutionClaimPhase::RepairPending,
        ExecutionClaimPhase::Failed,
        ExecutionClaimPhase::Revoked,
        ExecutionClaimPhase::Landed,
    ]
    .into_iter()
    .find(|phase| task::execution_claim_phase_label(*phase) == label)
}

/// What the phase means for an operator, in the words the lifecycle uses.
fn phase_summary(phase: ExecutionClaimPhase) -> &'static str {
    match phase {
        ExecutionClaimPhase::Claimed => "admitted; no leaf run is bound yet",
        ExecutionClaimPhase::Running => "executing on its bound run",
        ExecutionClaimPhase::HandedOff => {
            "delivery handed off and awaiting completion authority — this is not a code review"
        }
        ExecutionClaimPhase::RepairPending => {
            "landing stopped on its base; awaiting the one automatic repair"
        }
        ExecutionClaimPhase::Failed => "settled as failed; the task is blocked with its evidence",
        ExecutionClaimPhase::Revoked => {
            "revoked by deliberate recovery, or settled from its pull request merged outside Orbit"
        }
        ExecutionClaimPhase::Landed => "merged and completed against verified evidence",
    }
}

fn location_json(location: &orbit_types::task::ExecutionLocation) -> serde_json::Value {
    serde_json::json!({
        "known": true,
        "machine_id": location.machine_id,
        "machine_name": location.machine_name,
    })
}

pub(super) fn claim_json(
    claim: &ClaimInspection,
    handoff: Option<serde_json::Value>,
    local_machine: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    let executed_on = &claim.claim.executed_on;
    // A bound run lives in the executor's own job store. Unless this process can
    // *prove* it is that machine, there is no owner-local run to open — so say
    // where to look instead of minting a link that would resolve against the
    // wrong host's run id. An unknown local identity is not a match.
    let remote = local_machine != Some(executed_on.machine_id.as_str());
    let expires_at = chrono::DateTime::parse_from_rfc3339(&claim.claim.reservation_expires_at)
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc));
    // The reservation only guards an unsettled claim. Once the claim settles
    // (landed, failed, revoked) the reservation is released, so an elapsed
    // window on a settled claim is not an expiry worth reporting.
    let unsettled = claim.claim.phase.is_unsettled();
    let expired = unsettled && expires_at.is_some_and(|expires_at| expires_at <= now);
    serde_json::json!({
        "claim_id": claim.claim.claim_id,
        "task_id": claim.claim.task_id,
        "request_id": claim.claim.request_id,
        "phase": task::execution_claim_phase_label(claim.claim.phase),
        "phase_summary": phase_summary(claim.claim.phase),
        "authorizes_execution": matches!(
            claim.claim.phase,
            ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
        ),
        "unsettled": claim.claim.phase.is_unsettled(),
        "executed_on": location_json(executed_on),
        "run_context": {
            "run_id": claim.claim.run_context.run_id,
            "job_name": claim.claim.run_context.job_name,
            "machine_name": claim.claim.run_context.machine_name,
        },
        "bound_run": claim.bound_run.as_ref().map(|run| serde_json::json!({
            "machine_id": run.machine_id,
            "run_id": run.run_id,
        })),
        // Navigation, not authority: `#runs?run_id=` resolves against this
        // checkout's own job store.
        "bound_run_navigable": !remote && claim.bound_run.is_some(),
        "inspect_on": if remote {
            serde_json::Value::String(format!(
                "inspect this run on machine {} (no owner-local run exists)",
                executed_on.machine_id
            ))
        } else {
            serde_json::Value::Null
        },
        "footprint": claim.claim.footprint,
        // The frozen footprint keeps protecting these paths after expiry; an
        // expired reservation is a diagnostic, never a revocation.
        "footprint_protected": claim.claim.phase.protects_footprint(),
        "reservation": {
            "id": claim.claim.reservation_id,
            "expires_at": claim.claim.reservation_expires_at,
            "expired": expired,
            "note": if !unsettled {
                "reservation released — the claim has settled and no longer holds its footprint"
            } else if expired {
                "reservation window elapsed — the claim is still live and its frozen footprint \
                 still protects these files; expiry is not revocation and not proof the attempt died"
            } else {
                "reservation window is current"
            },
        },
        "created_at": claim.created_at,
        "updated_at": claim.updated_at,
        "last_event": claim.last_event,
        "age_seconds": claim.age_seconds,
        "unresolved_merge_intent": claim.unresolved_merge_intent,
        "landing_invalidated": claim.landing_invalidated,
        "handoff": handoff,
    })
}

pub(super) fn handoff_json(
    accepted: &AcceptedHandoff,
    request: Option<&LandingStartRequest>,
    attempt: Option<&LandingAttempt>,
    unresolved_merge_intent: Option<&str>,
) -> serde_json::Value {
    let candidate = &accepted.handoff.candidate;
    let no_diff = matches!(
        candidate.delivery,
        orbit_types::workflow::handoff::HandoffDelivery::NoDiff { .. }
    );
    let revoked = request.is_some_and(|request| request.state == LandingStartState::Revoked);
    // Nothing lands a revoked handoff and the revocation is final, so the
    // console names the way out: a pull request merged outside Orbit settles
    // the task through `settle-merged`; otherwise the claim is recovered.
    let settle_command = matches!(
        candidate.delivery,
        orbit_types::workflow::handoff::HandoffDelivery::PullRequest { .. }
    )
    .then(|| format!("orbit task settle-merged {}", accepted.handoff.task_id))
    .filter(|_| revoked);
    let (authority_state, authority_summary) = match request.map(|request| request.state) {
        None => (
            "not_authorized",
            "no completion authority is recorded; this handoff waits for explicit owner approval"
                .to_string(),
        ),
        Some(LandingStartState::Pending) => (
            "authorized",
            "completion authority recorded; the owner landing job carries it from here".to_string(),
        ),
        Some(LandingStartState::Revoked) => (
            "revoked",
            match &settle_command {
                Some(command) => format!(
                    "completion authority was withdrawn; the task stays in review. If its pull \
                     request merged outside Orbit, `{command}` completes the task; otherwise \
                     recover the claim to another status"
                ),
                None => "completion authority was withdrawn; the task stays in review. Recover \
                         the claim to another status"
                    .to_string(),
            },
        ),
        Some(LandingStartState::Completed) if no_diff => (
            "completed",
            "authority was consumed by verified no-diff completion".to_string(),
        ),
        Some(LandingStartState::Completed) => (
            "completed",
            "authority was consumed by a verified merge".to_string(),
        ),
    };
    let (landing_state, landing_summary) = match attempt.map(|attempt| attempt.state) {
        None => ("none", "no landing attempt has been reserved"),
        Some(LandingAttemptState::Dispatched) => (
            "dispatched",
            "an owner landing job is carrying this handoff",
        ),
        Some(LandingAttemptState::Merged) if no_diff => (
            "completed",
            "verified clean-base delivery completed without an external merge",
        ),
        // Merged is merged: the candidate is on the landing branch. It says
        // nothing about whether anything was deployed.
        Some(LandingAttemptState::Merged) => (
            "merged",
            "merged into the landing branch against verified evidence — merged is not deployed",
        ),
        Some(LandingAttemptState::Stopped) => (
            "stopped",
            "stopped with durable evidence; an operator may retry this authorized handoff; candidate repairs require a new validated handoff",
        ),
    };
    serde_json::json!({
        "handoff_id": accepted.handoff_id,
        "accepted_at": accepted.accepted_at.to_rfc3339(),
        "task_id": accepted.handoff.task_id,
        "claim_id": accepted.handoff.claim_id,
        "executed_on": {
            "known": true,
            "machine_id": accepted.handoff.machine_id,
            "machine_name": serde_json::Value::Null,
        },
        "run_id": accepted.handoff.run_id,
        "execution_summary": accepted.handoff.execution_summary,
        "candidate": {
            "repository": candidate.repository,
            "source_branch": candidate.source_branch,
            "base_branch": candidate.base_branch,
            "landing_branch": candidate.landing_branch,
            "candidate": {"commit": candidate.candidate.commit, "tree": candidate.candidate.tree},
            "base": {"commit": candidate.base.commit, "tree": candidate.base.tree},
            "delivery": candidate.delivery,
        },
        // The typed disposition records what the claimed leaf's review
        // settled [ORB-13908]: a before-PR verdict the owner checked at
        // acceptance, or that no review was required — which is not a review
        // that passed. Either way the task's `review` status means "delivery
        // awaiting completion authority".
        "review": {
            "policy": accepted.handoff.review.policy,
            "disposition": accepted.handoff.review.disposition,
            "is_code_review": accepted.handoff.review.evidence().is_some(),
            "summary": review_summary(&accepted.handoff.review),
        },
        "required_commands": accepted.required_commands,
        "validation": accepted.handoff.validation,
        "authority": {
            "state": authority_state,
            "summary": authority_summary,
            "authorization_id": request.map(|request| request.authorization_id.clone()),
            "recorded_at": request.map(|request| request.created_at.to_rfc3339()),
            // The command that completes a revoked handoff's task once its
            // pull request merged outside Orbit; null in every other state.
            "settle_command": settle_command,
        },
        "landing": {
            "state": landing_state,
            "summary": landing_summary,
            "attempt": attempt.map(|attempt| attempt.attempt),
            "job_run_id": attempt.and_then(|attempt| attempt.job_run_id.clone()),
            "evidence": attempt.and_then(|attempt| attempt.evidence.clone()),
            "merged": !no_diff && matches!(attempt.map(|attempt| attempt.state), Some(LandingAttemptState::Merged)),
            "deployed": serde_json::Value::Null,
        },
        // An external send whose reply was lost. Until it is reconciled against
        // the provider, neither revocation nor recovery may proceed: a database
        // row cannot cancel a request GitHub may already have applied.
        "uncertain_merge_intent": unresolved_merge_intent,
    })
}

/// One line on what a handoff's review settled, for the owner console.
fn review_summary(review: &orbit_types::workflow::handoff::HandoffReview) -> String {
    match review.evidence() {
        Some(evidence) => format!(
            "{} review {} by crew `{}` (attempt {}) on candidate {}; the owner checked its \
             certificate at acceptance",
            match review.evidence_timing() {
                orbit_types::workflow::ReviewTiming::BeforeLanding => "before-landing",
                _ => "before-PR",
            },
            evidence.verdict.as_str(),
            evidence.reviewer_crew,
            evidence.attempt_id,
            evidence.reviewed_head_sha
        ),
        None => {
            "review not required (policy none) — no reviewer ran and no verdict exists".to_string()
        }
    }
}
