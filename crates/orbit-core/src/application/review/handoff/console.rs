//! The owner operator console: claim provenance reads and the operator
//! entry points that settle a handoff.

use std::collections::HashMap;

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_store::contracts::{
    ClaimInspection, ClaimInvocation, ClaimMutation, ExecutionClaimPhase, HandoffObservation,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::handoff::AcceptedHandoff;

use crate::OrbitRuntime;
use crate::application::contracts::DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED;
use crate::application::job::JobDrainPort;
use crate::application::operator_input;
use crate::application::task;

use super::candidate::ExpectedCandidate;
use super::console_json::{
    claim_json, handoff_json, mutation_json, phase_from_label, settled_claim_summary_json,
};

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
pub(crate) const HANDOFF_CONSOLE_SCHEMA: u32 = 1;

/// Claim lifecycle states selected by the owner's read-only console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistributedClaimState {
    /// Claims that still hold execution authority or a protected footprint.
    Active,
    /// Claims whose execution lifecycle has settled.
    Settled,
    /// Both active and settled claims.
    All,
}

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

    /// The console refusal for an owner claim refusal of `kind`.
    ///
    /// Every fencing failure reads as stale, and an unreconciled external send
    /// is named separately, because the two need different operator responses:
    /// refresh, versus reconcile the merge first.
    pub fn for_claim_refusal(kind: ClaimRefusalKind) -> Self {
        match kind {
            ClaimRefusalKind::StaleClaim
            | ClaimRefusalKind::HandoffCandidateMismatch
            | ClaimRefusalKind::HandoffIdentityMismatch
            | ClaimRefusalKind::ValidationRequirementsChanged
            | ClaimRefusalKind::LandingAuthorityRevoked
            | ClaimRefusalKind::HandoffAlreadyLanded => Self::Stale,
            ClaimRefusalKind::NotCurrent => Self::NotCurrent,
            ClaimRefusalKind::UnresolvedMergeIntent
            | ClaimRefusalKind::MergeIntentReplayUnreconciled => Self::UncertainMerge,
        }
    }
}

impl OrbitRuntime {
    /// Classify a refusal one of this runtime's console reads or actions
    /// returned, for the console to surface verbatim to an operator.
    ///
    /// Decided from the error's variant and this runtime's role, never from
    /// its text: a capability refusal here is the replica-checkout refusal
    /// exactly when this runtime is a replica, and an owner claim refusal maps
    /// by its kind.
    pub fn handoff_console_refusal(&self, error: &OrbitError) -> Option<HandoffConsoleRefusal> {
        if matches!(error, OrbitError::CapabilityRefused(_))
            && self.replica_owner_machine().is_some()
        {
            return Some(HandoffConsoleRefusal::ReplicaCheckout);
        }
        error
            .claim_refusal()
            .map(HandoffConsoleRefusal::for_claim_refusal)
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
        self.distributed_claim_console_at(chrono::Utc::now())
    }

    /// Read the owner claim console with bounded task and lifecycle selection.
    /// Settled claims are compact summaries unless `detail` is true.
    pub fn distributed_claim_console_filtered(
        &self,
        task_id: Option<&str>,
        state: DistributedClaimState,
        detail: bool,
    ) -> Result<serde_json::Value, OrbitError> {
        self.distributed_claim_console_filtered_at(task_id, state, detail, chrono::Utc::now())
    }

    /// [`Self::distributed_claim_console`] observed at `now`, so a reservation
    /// expiry can be projected without waiting out its real TTL.
    pub(crate) fn distributed_claim_console_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<serde_json::Value, OrbitError> {
        self.distributed_claim_console_filtered_at(None, DistributedClaimState::All, true, now)
    }

    fn distributed_claim_console_filtered_at(
        &self,
        task_id: Option<&str>,
        state: DistributedClaimState,
        detail: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<serde_json::Value, OrbitError> {
        if let Err(error) = self.ensure_coordination_task_write_permitted() {
            return Ok(serde_json::json!({
                "schema_version": HANDOFF_CONSOLE_SCHEMA,
                "owner_workspace": false,
                "refusal": HandoffConsoleRefusal::ReplicaCheckout.code(),
                "refusal_detail": error.to_string(),
                "distributed_execution_enabled":
                    DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED,
                "claims": Vec::<serde_json::Value>::new(),
            }));
        }
        let inspected_claims = match (task_id, state) {
            (Some(task_id), _) => self
                .stores()
                .tasks()
                .inspect_task_execution_claims(task_id)?,
            (None, DistributedClaimState::Active) => {
                self.stores().tasks().inspect_live_execution_claims()?
            }
            (None, DistributedClaimState::Settled | DistributedClaimState::All) => {
                self.stores().tasks().inspect_execution_claims()?
            }
        };
        let claims = inspected_claims
            .iter()
            .filter(|claim| {
                task_id.is_none_or(|task_id| claim.claim.task_id == task_id)
                    && match state {
                        DistributedClaimState::Active => claim.claim.phase.is_unsettled(),
                        DistributedClaimState::Settled => !claim.claim.phase.is_unsettled(),
                        DistributedClaimState::All => true,
                    }
            })
            .collect::<Vec<_>>();
        let local_machine = self.automation_machine_identity().map(str::to_string);

        // Read accepted handoffs once and join in memory. The old projection did
        // one indexed store read per claim and linearly searched landing records
        // twice for each accepted handoff.
        let needs_detail = claims
            .iter()
            .any(|claim| claim.claim.phase.is_unsettled() || detail);
        let accepted = if needs_detail {
            self.stores().tasks().accepted_handoffs()?
        } else {
            Vec::new()
        };
        let accepted_by_claim = accepted
            .iter()
            .map(|accepted| (accepted.handoff.claim_id.as_str(), accepted))
            .collect::<HashMap<_, _>>();
        let has_selected_handoffs = claims.iter().any(|claim| {
            (claim.claim.phase.is_unsettled() || detail)
                && accepted_by_claim.contains_key(claim.claim.claim_id.as_str())
        });
        let requests = if has_selected_handoffs {
            self.stores().tasks().landing_start_requests()?
        } else {
            Vec::new()
        };
        let requests_by_handoff = requests
            .iter()
            .map(|request| (request.handoff_id.as_str(), request))
            .collect::<HashMap<_, _>>();
        let attempts = if has_selected_handoffs {
            self.stores().tasks().landing_attempts()?
        } else {
            Vec::new()
        };
        let attempts_by_handoff = attempts
            .iter()
            .map(|attempt| (attempt.handoff_id.as_str(), attempt))
            .collect::<HashMap<_, _>>();

        let mut rows = Vec::with_capacity(claims.len());
        for claim in &claims {
            if !claim.claim.phase.is_unsettled() && !detail {
                rows.push(settled_claim_summary_json(claim));
                continue;
            }
            let handoff = accepted_by_claim
                .get(claim.claim.claim_id.as_str())
                .map(|accepted| {
                    let request = requests_by_handoff
                        .get(accepted.handoff_id.as_str())
                        .copied();
                    let attempt = attempts_by_handoff
                        .get(accepted.handoff_id.as_str())
                        .copied();
                    handoff_json(
                        accepted,
                        request,
                        attempt,
                        claim.unresolved_merge_intent.as_deref(),
                    )
                });
            let mut row = claim_json(claim, handoff, local_machine.as_deref(), now);
            let current = self.claim_is_current_on_task(claim);
            row["current"] = serde_json::json!(current);
            row["recover_targets"] = self.claim_recover_targets_json(claim, current)?;
            rows.push(row);
        }

        Ok(serde_json::json!({
            "schema_version": HANDOFF_CONSOLE_SCHEMA,
            "owner_workspace": true,
            "refusal": serde_json::Value::Null,
            "refusal_detail": serde_json::Value::Null,
            "distributed_execution_enabled":
                DISTRIBUTED_MUTATION_ENTRY_POINTS_ENABLED,
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
            footprint_widening: accepted.handoff.footprint_widening.clone(),
            candidate: accepted.handoff.candidate.clone(),
            required_commands: accepted.required_commands.clone(),
            owner_completion_authority: self.handoff_completion_authority(),
            // Acceptance observed the review's base and repository; the store
            // rechecks the review evidence the handoff pinned without them.
            review: None,
            // Acceptance ran the known-red bases; approval rechecks the logs.
            known_red: Vec::new(),
            light_delivery: self
                .owner_light_delivery(&accepted.handoff.task_id, &accepted.handoff.candidate)?,
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
        replay_scope: &str,
        request_key: &str,
    ) -> Result<serde_json::Value, OrbitError> {
        let reason = operator_input::reason(reason, "revoking completion authority")?;
        let request_key = operator_input::replay_key(request_key)?;
        let request_id = operator_input::scoped_replay_key(replay_scope, handoff_id, &request_key);
        let expected = ExpectedCandidate {
            candidate_commit: expected.candidate_commit.trim().to_string(),
            base_commit: expected.base_commit.trim().to_string(),
        };
        let (claim, accepted) = self.console_handoff(handoff_id)?;
        expected.check(&accepted.handoff.candidate)?;
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            actor.to_string(),
        );
        let result =
            self.revoke_task_handoff(&context, &request_id, accepted.handoff_id.clone(), reason)?;
        Ok(mutation_json(&accepted.handoff_id, &result))
    }

    /// Deliberate recovery of one claim: fence the attempt and choose the task
    /// transition — any status the lifecycle allows from the task's current
    /// one except `in-progress`, with `done` held to the ordinary completion
    /// gates. Never automatic — there is no heartbeat and no inferred
    /// death, so this exists precisely because an operator has to decide.
    ///
    /// `expected_phase` is the phase the operator saw. A claim that moved on
    /// since the page was rendered is refused rather than recovered, unless
    /// this request is an identical replay of the recovery that moved it:
    /// the phase precondition is enforced inside the store transaction,
    /// after the recorded receipt has had its chance to replay.
    ///
    /// `replay` is `(replay_scope, request_key)`: the surface's replay
    /// namespace (`"operator-recover"` or `"dashboard-recover"`) and the
    /// caller's key for this one decision.
    pub fn recover_claim_as_operator(
        &self,
        claim_id: &str,
        expected_phase: &str,
        status: TaskStatus,
        actor: &str,
        reason: &str,
        replay: (&str, &str),
    ) -> Result<serde_json::Value, OrbitError> {
        let (replay_scope, request_key) = replay;
        let reason = operator_input::reason(reason, "claim recovery")?;
        let request_key = operator_input::replay_key(request_key)?;
        let request_id = operator_input::scoped_replay_key(replay_scope, claim_id, &request_key);
        let expected_phase = expected_phase.trim();
        self.ensure_coordination_task_write_permitted()?;
        let claim = self
            .stores()
            .tasks()
            .resolve_execution_claims()?
            .into_iter()
            .find(|claim| claim.claim.claim_id == claim_id)
            .ok_or_else(|| OrbitError::ClaimRefused {
                kind: ClaimRefusalKind::StaleClaim,
                message: format!("no claim '{claim_id}' is current on this owner (stale_claim)"),
            })?;
        // The target obeys the task lifecycle from the status the claim holds
        // the task in, and `done` passes the ordinary completion gates. The
        // store transaction pins that status, so a task that moved since this
        // check is refused there as a stale claim. A claim this recovery
        // already revoked skips the check, so an identical request replays
        // its receipt even after the task moved on.
        if claim.claim.phase != ExecutionClaimPhase::Revoked {
            task::ensure_claim_recovery_target(
                self,
                &self.get_task(&claim.claim.task_id)?,
                status,
            )?;
        }
        let observed = task::execution_claim_phase_label(claim.claim.phase);
        let Some(expected_phase) = phase_from_label(expected_phase) else {
            return Err(OrbitError::ClaimRefused {
                kind: ClaimRefusalKind::StaleClaim,
                message: format!(
                    "claim '{claim_id}' is now '{observed}', not the '{expected_phase}' this \
                     action was prepared against (stale_claim)"
                ),
            });
        };
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            actor.to_string(),
        );
        let result = self.mutate_execution_claim(
            Some(&context),
            &request_id,
            &ClaimMutation::Recover {
                status,
                reason: reason.to_string(),
                expected_phase,
            },
        )?;
        Ok(serde_json::json!({
            "claim_id": result.claim_id,
            "phase": task::execution_claim_phase_label(result.phase),
            "task_status": result.status.to_string(),
        }))
    }

    /// Use the same latest `pulled_by` identity as the store's claim authority,
    /// rather than a claim's phase or creation time. A failed claim can remain
    /// in history after a newer pull; unreadable provenance proves no currency.
    fn claim_is_current_on_task(&self, claim: &ClaimInspection) -> bool {
        let Ok(history) = self.get_task_history(&claim.claim.task_id) else {
            return false;
        };
        history
            .iter()
            .rev()
            .find(|entry| entry.event == "pulled_by")
            .and_then(|entry| {
                serde_json::from_str::<serde_json::Value>(entry.note.as_deref()?).ok()
            })
            .is_some_and(|note| note["claim_id"].as_str() == Some(claim.claim.claim_id.as_str()))
    }

    /// The statuses an operator may recover the current `claim` to, for the console's
    /// picker; empty when the claim cannot be recovered. Advisory: the
    /// guarded recovery rechecks every target.
    fn claim_recover_targets_json(
        &self,
        claim: &ClaimInspection,
        current: bool,
    ) -> Result<serde_json::Value, OrbitError> {
        let recoverable =
            claim.claim.phase.is_unsettled() || claim.claim.phase == ExecutionClaimPhase::Failed;
        if !current || !recoverable {
            return Ok(serde_json::json!([]));
        }
        // A task this owner cannot read offers no target rather than failing
        // the whole console; the recovery itself reports why.
        let Ok(task) = self.get_task(&claim.claim.task_id) else {
            return Ok(serde_json::json!([]));
        };
        let targets = task::claim_recovery_targets(self, &task);
        Ok(serde_json::json!(
            targets.iter().map(ToString::to_string).collect::<Vec<_>>()
        ))
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
            let Some(accepted) = self
                .stores()
                .tasks()
                .find_accepted_handoff(&claim.claim.claim_id)?
            else {
                continue;
            };
            if accepted.handoff_id == handoff_id {
                return Ok((claim, accepted));
            }
        }
        Err(OrbitError::ClaimRefused {
            kind: ClaimRefusalKind::NotCurrent,
            message: format!("no accepted handoff '{handoff_id}' is current on this owner"),
        })
    }
}
