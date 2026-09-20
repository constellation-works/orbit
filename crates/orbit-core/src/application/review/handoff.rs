//! Owner-domain handoff seam. Public distributed mutation tools remain gated.
//! Trusted callers obtain observations from Git/provider state and repository check
//! policy, including the existing already-landed verifier for no-diff work. They
//! must never manufacture observations by copying the worker's handoff payload.
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimInspection, ClaimInvocation, ClaimMutation, ClaimMutationResult, ExecutionClaimPhase,
    HandoffObservation,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::handoff::{
    AcceptedHandoff, HandoffCandidate, LandingAttempt, LandingAttemptState, LandingStartRequest,
    LandingStartState, TaskHandoff,
};

use crate::OrbitRuntime;
use crate::application::landing::dispatch_recorded_authority;

impl OrbitRuntime {
    pub fn accepted_task_handoff(
        &self,
        claim_id: &str,
    ) -> Result<orbit_types::workflow::handoff::AcceptedHandoff, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().accepted_handoff(claim_id)
    }

    /// Accepting a completion-authorized handoff records its landing-start
    /// request; the owner landing job is dispatched from that request here, so
    /// no drain or ship sweep has to be running for authorized work to land.
    /// A review-only handoff records no request and dispatches nothing.
    pub fn accept_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff: TaskHandoff,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let context = context.clone().with_handoff_observation(observation);
        let result = self.mutate_execution_claim(
            Some(&context),
            request_id,
            &ClaimMutation::AcceptHandoff(handoff),
        )?;
        dispatch_recorded_authority(self);
        Ok(result)
    }

    /// Explicit review-state approval. Does not reuse the backlog grant validator.
    pub fn approve_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff_id: String,
        candidate: HandoffCandidate,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let context = context.clone().with_handoff_observation(observation);
        let result = self.mutate_execution_claim(
            Some(&context),
            request_id,
            &ClaimMutation::ApproveHandoff {
                handoff_id,
                candidate,
            },
        )?;
        dispatch_recorded_authority(self);
        Ok(result)
    }

    pub fn revoke_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff_id: String,
        reason: String,
    ) -> Result<ClaimMutationResult, OrbitError> {
        self.mutate_execution_claim(
            Some(context),
            request_id,
            &ClaimMutation::RevokeHandoff { handoff_id, reason },
        )
    }

    /// The durable outbox, readable without a live drain or ship sweep. Dispatch
    /// and external merge reconciliation belong to
    /// [`crate::application::landing`].
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().landing_start_requests()
    }
}

// ---------------------------------------------------------------------------
// Owner operator console [ORB-12516]
// ---------------------------------------------------------------------------
//
// One read projection and three operator entry points, so an authorized owner
// surface (today: the dashboard) can inspect claim provenance and settle a
// handoff without reaching into `orbit-store` itself. The adapter crates above
// Core cannot depend on the store — `ClaimInvocation` and `HandoffObservation`
// are deliberately not constructible from outside trusted runtime code — so the
// observation each approval commits is rebuilt *here*, from the owner's own
// accepted record, and the caller's candidate is only ever an expectation that
// must match it.
//
// Nothing here is a second lifecycle engine: every mutation ends in the same
// `approve_task_handoff` / `revoke_task_handoff` / `ClaimMutation::Recover`
// transaction the landing consumer uses, and every refusal is the store's.

/// Response shape version of [`OrbitRuntime::distributed_claim_console`].
pub const HANDOFF_CONSOLE_SCHEMA: u32 = 1;

/// Why an owner console read or action was refused before the store saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffConsoleRefusal {
    /// This checkout is a replica; the owner machine serves claim state.
    ReplicaCheckout,
    /// No claim currently carries this handoff or claim id.
    NotCurrent,
    /// The caller's expected identity is not the one the owner holds.
    Stale,
    /// A merge intent was recorded externally and has not been reconciled.
    UncertainMerge,
}

impl HandoffConsoleRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::ReplicaCheckout => "replica_checkout",
            Self::NotCurrent => "handoff_not_current",
            Self::Stale => "stale_claim",
            Self::UncertainMerge => "uncertain_merge_intent",
        }
    }

    /// Classify a store refusal the console surfaces verbatim to an operator.
    ///
    /// The store speaks one vocabulary for every fencing failure (`stale_claim`)
    /// and names an unreconciled external send separately, because the two need
    /// different operator responses: refresh, versus reconcile the merge first.
    pub fn classify(error: &OrbitError) -> Option<Self> {
        let message = error.to_string();
        if message.contains("unresolved external merge intent")
            || message.contains("merge intent replay requires reconciliation")
        {
            return Some(Self::UncertainMerge);
        }
        if matches!(error, OrbitError::CapabilityRefused(_)) && message.contains("replica checkout")
        {
            return Some(Self::ReplicaCheckout);
        }
        if message.contains("stale_claim")
            || message.contains("handoff candidate mismatch")
            || message.contains("handoff identity mismatch")
            || message.contains("validation requirements changed")
            || message.contains("landing authority revoked")
            || message.contains("handoff has already landed")
        {
            return Some(Self::Stale);
        }
        None
    }
}

impl OrbitRuntime {
    /// Read-only owner view of every live claim, its execution provenance, its
    /// accepted handoff and that handoff's authority and landing state.
    ///
    /// Creates nothing. A replica checkout is answered rather than errored — it
    /// simply reports that the owner machine holds this state — so a dashboard
    /// switching workspaces renders an honest empty view instead of a fault.
    /// Every diagnostic here is a diagnostic: an expired reservation, an absent
    /// owner-local run and a claim's age are *not* evidence that the attempt
    /// died, and the projection never labels them as revocation.
    pub fn distributed_claim_console(&self) -> Result<serde_json::Value, OrbitError> {
        if let Err(error) = self.ensure_coordination_task_write_permitted() {
            return Ok(serde_json::json!({
                "schema_version": HANDOFF_CONSOLE_SCHEMA,
                "owner_workspace": false,
                "refusal": HandoffConsoleRefusal::ReplicaCheckout.code(),
                "refusal_detail": error.to_string(),
                "distributed_execution_enabled":
                    crate::application::distributed::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED,
                "claims": Vec::<serde_json::Value>::new(),
            }));
        }
        let claims = self.stores().tasks().inspect_execution_claims()?;
        let requests = self.stores().tasks().landing_start_requests()?;
        let attempts = self.stores().tasks().landing_attempts()?;
        let local_machine = self.automation_machine_identity().map(str::to_string);
        let now = chrono::Utc::now();

        let mut rows = Vec::with_capacity(claims.len());
        for claim in &claims {
            let accepted = self
                .stores()
                .tasks()
                .accepted_handoff(&claim.claim.claim_id)
                .ok();
            let handoff = accepted.as_ref().map(|accepted| {
                let request = requests
                    .iter()
                    .find(|request| request.handoff_id == accepted.handoff_id);
                let attempt = attempts
                    .iter()
                    .find(|attempt| attempt.handoff_id == accepted.handoff_id);
                handoff_json(
                    accepted,
                    request,
                    attempt,
                    claim.unresolved_merge_intent.as_deref(),
                )
            });
            rows.push(claim_json(claim, handoff, local_machine.as_deref(), now));
        }

        Ok(serde_json::json!({
            "schema_version": HANDOFF_CONSOLE_SCHEMA,
            "owner_workspace": true,
            "refusal": serde_json::Value::Null,
            "refusal_detail": serde_json::Value::Null,
            "distributed_execution_enabled":
                crate::application::distributed::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED,
            "claims": rows,
        }))
    }

    /// Approve one named handoff as the owner operator.
    ///
    /// `expected` pins what the operator was looking at. It is compared against
    /// the owner's stored record and then discarded: the candidate and the
    /// required-command list that reach the store come from the accepted
    /// handoff, so a manipulated payload cannot widen what is approved. The
    /// store rechecks the authorization, the candidate and the digest-pinned
    /// validation evidence inside the same transaction.
    pub fn approve_handoff_as_operator(
        &self,
        handoff_id: &str,
        expected: &ExpectedCandidate,
        approver: &str,
        request_id: &str,
    ) -> Result<serde_json::Value, OrbitError> {
        let (claim, accepted) = self.console_handoff(handoff_id)?;
        expected.check(&accepted.handoff.candidate)?;
        let observation = HandoffObservation {
            candidate: accepted.handoff.candidate.clone(),
            required_commands: accepted.required_commands.clone(),
        };
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            approver.to_string(),
        );
        let result = self.approve_task_handoff(
            &context,
            request_id,
            accepted.handoff_id.clone(),
            accepted.handoff.candidate.clone(),
            observation,
        )?;
        Ok(mutation_json(&accepted.handoff_id, &result))
    }

    /// Withdraw completion authority for one named handoff. The task stays in
    /// `review`: revocation cancels the pending landing request, it does not
    /// decide what happens to the work.
    pub fn revoke_handoff_as_operator(
        &self,
        handoff_id: &str,
        expected: &ExpectedCandidate,
        actor: &str,
        reason: &str,
        request_id: &str,
    ) -> Result<serde_json::Value, OrbitError> {
        if reason.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "revoking completion authority requires a reason".to_string(),
            ));
        }
        let (claim, accepted) = self.console_handoff(handoff_id)?;
        expected.check(&accepted.handoff.candidate)?;
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            actor.to_string(),
        );
        let result = self.revoke_task_handoff(
            &context,
            request_id,
            accepted.handoff_id.clone(),
            reason.to_string(),
        )?;
        Ok(mutation_json(&accepted.handoff_id, &result))
    }

    /// Deliberate recovery of one claim: fence the attempt and choose the task
    /// transition. Never automatic — there is no heartbeat and no inferred
    /// death, so this exists precisely because an operator has to decide.
    ///
    /// `expected_phase` is the phase the operator saw. A claim that moved on
    /// since the page was rendered is refused rather than recovered.
    pub fn recover_claim_as_operator(
        &self,
        claim_id: &str,
        expected_phase: &str,
        status: TaskStatus,
        actor: &str,
        reason: &str,
        request_id: &str,
    ) -> Result<serde_json::Value, OrbitError> {
        if reason.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "claim recovery requires a reason".to_string(),
            ));
        }
        if !matches!(status, TaskStatus::Blocked | TaskStatus::Backlog) {
            return Err(OrbitError::InvalidInput(format!(
                "claim recovery moves a task to blocked or backlog, not '{status}'"
            )));
        }
        self.ensure_coordination_task_write_permitted()?;
        let claim = self
            .stores()
            .tasks()
            .resolve_execution_claims()?
            .into_iter()
            .find(|claim| claim.claim.claim_id == claim_id)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "no claim '{claim_id}' is current on this owner (stale_claim)"
                ))
            })?;
        let observed = phase_label(claim.claim.phase);
        if observed != expected_phase {
            return Err(OrbitError::InvalidInput(format!(
                "claim '{claim_id}' is now '{observed}', not the '{expected_phase}' this action \
                 was prepared against (stale_claim)"
            )));
        }
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            actor.to_string(),
        );
        let result = self.mutate_execution_claim(
            Some(&context),
            request_id,
            &ClaimMutation::Recover {
                status,
                reason: reason.to_string(),
            },
        )?;
        Ok(serde_json::json!({
            "claim_id": result.claim_id,
            "phase": phase_label(result.phase),
            "task_status": result.status.to_string(),
        }))
    }

    /// Resolve a handoff id to the claim that currently owns it.
    ///
    /// Shares [`Self::land_handoff`]'s rule: handoff identity is a digest of the
    /// accepted record, so this is an exact match over the few live claims
    /// rather than a search over task state. The repairing read is deliberate —
    /// an interrupted coordination commit is settled before an operator decides
    /// anything from what it shows.
    fn console_handoff(
        &self,
        handoff_id: &str,
    ) -> Result<(ClaimInspection, AcceptedHandoff), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        for claim in self.stores().tasks().resolve_execution_claims()? {
            let Ok(accepted) = self
                .stores()
                .tasks()
                .accepted_handoff(&claim.claim.claim_id)
            else {
                continue;
            };
            if accepted.handoff_id == handoff_id {
                return Ok((claim, accepted));
            }
        }
        Err(OrbitError::InvalidInput(format!(
            "no accepted handoff '{handoff_id}' is current on this owner"
        )))
    }
}

/// The candidate identity an operator surface believed it was acting on.
///
/// Deliberately narrower than [`HandoffCandidate`]: a handoff is immutable, so
/// the commit pair is enough to prove the caller and the owner are looking at
/// the same accepted record, and a browser never has to round-trip a
/// `deny_unknown_fields` struct it did not author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCandidate {
    pub candidate_commit: String,
    pub base_commit: String,
}

impl ExpectedCandidate {
    fn check(&self, candidate: &HandoffCandidate) -> Result<(), OrbitError> {
        if self.candidate_commit == candidate.candidate.commit
            && self.base_commit == candidate.base.commit
        {
            return Ok(());
        }
        Err(OrbitError::InvalidInput(format!(
            "this action was prepared against candidate {} on base {}; the owner now holds \
             candidate {} on base {} (stale_claim)",
            self.candidate_commit,
            self.base_commit,
            candidate.candidate.commit,
            candidate.base.commit,
        )))
    }
}

fn mutation_json(handoff_id: &str, result: &ClaimMutationResult) -> serde_json::Value {
    serde_json::json!({
        "handoff_id": handoff_id,
        "claim_id": result.claim_id,
        "phase": phase_label(result.phase),
        "task_status": result.status.to_string(),
    })
}

fn phase_label(phase: ExecutionClaimPhase) -> &'static str {
    match phase {
        ExecutionClaimPhase::Claimed => "claimed",
        ExecutionClaimPhase::Running => "running",
        ExecutionClaimPhase::HandedOff => "handed_off",
        ExecutionClaimPhase::Failed => "failed",
        ExecutionClaimPhase::Revoked => "revoked",
        ExecutionClaimPhase::Landed => "landed",
    }
}

/// What the phase means for an operator, in the words the lifecycle uses.
fn phase_summary(phase: ExecutionClaimPhase) -> &'static str {
    match phase {
        ExecutionClaimPhase::Claimed => "admitted; no leaf run is bound yet",
        ExecutionClaimPhase::Running => "executing on its bound run",
        ExecutionClaimPhase::HandedOff => {
            "delivery handed off and awaiting completion authority — this is not a code review"
        }
        ExecutionClaimPhase::Failed => "settled as failed; the task is blocked with its evidence",
        ExecutionClaimPhase::Revoked => "revoked by deliberate recovery",
        ExecutionClaimPhase::Landed => "merged and completed against verified evidence",
    }
}

fn location_json(location: &orbit_types::task::ExecutionLocation) -> serde_json::Value {
    serde_json::json!({
        "known": true,
        "machine_id": location.machine_id,
        "host_id": location.host_id,
    })
}

fn claim_json(
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
    let expired = expires_at.is_some_and(|expires_at| expires_at <= now);
    serde_json::json!({
        "claim_id": claim.claim.claim_id,
        "task_id": claim.claim.task_id,
        "request_id": claim.claim.request_id,
        "phase": phase_label(claim.claim.phase),
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
            "host_id": claim.claim.run_context.host_id,
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
            "note": if expired {
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

fn handoff_json(
    accepted: &AcceptedHandoff,
    request: Option<&LandingStartRequest>,
    attempt: Option<&LandingAttempt>,
    unresolved_merge_intent: Option<&str>,
) -> serde_json::Value {
    let candidate = &accepted.handoff.candidate;
    let (authority_state, authority_summary) = match request.map(|request| request.state) {
        None => (
            "not_authorized",
            "no completion authority is recorded; this handoff waits for explicit owner approval",
        ),
        Some(LandingStartState::Pending) => (
            "authorized",
            "completion authority recorded; the owner landing job carries it from here",
        ),
        Some(LandingStartState::Revoked) => (
            "revoked",
            "completion authority was withdrawn; the task stays in review",
        ),
        Some(LandingStartState::Completed) => {
            ("completed", "authority was consumed by a verified merge")
        }
    };
    let (landing_state, landing_summary) = match attempt.map(|attempt| attempt.state) {
        None => ("none", "no landing attempt has been reserved"),
        Some(LandingAttemptState::Dispatched) => (
            "dispatched",
            "an owner landing job is carrying this handoff",
        ),
        // Merged is merged: the candidate is on the landing branch. It says
        // nothing about whether anything was deployed.
        Some(LandingAttemptState::Merged) => (
            "merged",
            "merged into the landing branch against verified evidence — merged is not deployed",
        ),
        Some(LandingAttemptState::Stopped) => (
            "stopped",
            "stopped with durable evidence; a repair needs fresh validation and a new handoff",
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
            "host_id": serde_json::Value::Null,
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
        // v1 admits `review_policy = none` only. The typed disposition records
        // that no review was required — it is not a review that passed, and the
        // task's `review` status means "delivery awaiting completion authority".
        "review": {
            "policy": accepted.handoff.review.policy,
            "disposition": accepted.handoff.review.disposition,
            "is_code_review": false,
            "summary": "review not required (policy none) — no reviewer ran and no verdict exists",
        },
        "required_commands": accepted.required_commands,
        "validation": accepted.handoff.validation,
        "authority": {
            "state": authority_state,
            "summary": authority_summary,
            "authorization_id": request.map(|request| request.authorization_id.clone()),
            "recorded_at": request.map(|request| request.created_at.to_rfc3339()),
        },
        "landing": {
            "state": landing_state,
            "summary": landing_summary,
            "attempt": attempt.map(|attempt| attempt.attempt),
            "job_run_id": attempt.and_then(|attempt| attempt.job_run_id.clone()),
            "evidence": attempt.and_then(|attempt| attempt.evidence.clone()),
            "merged": matches!(attempt.map(|attempt| attempt.state), Some(LandingAttemptState::Merged)),
            "deployed": serde_json::Value::Null,
        },
        // An external send whose reply was lost. Until it is reconciled against
        // the provider, neither revocation nor recovery may proceed: a database
        // row cannot cancel a request GitHub may already have applied.
        "uncertain_merge_intent": unresolved_merge_intent,
    })
}
