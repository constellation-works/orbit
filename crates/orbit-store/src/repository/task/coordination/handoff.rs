//! Typed handoff acceptance and completion authority on the claim journal.
use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::{ReviewTiming, handoff::*};
use sha2::{Digest, Sha256};

use super::TaskCommitBoundary;
use super::lifecycle::{decode, encode, invalid, row};
use crate::contracts::*;

const HANDOFF: &str = "distributed-handoff-v1";
const AUTHORIZATION: &str = "distributed-handoff-authorization-v1";
const REVOCATION: &str = "distributed-handoff-revocation-v1";
const START: &str = "distributed-landing-start-v1";

impl TaskCommitBoundary {
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(START)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    pub fn accepted_handoff(&self, claim_id: &str) -> Result<AcceptedHandoff, OrbitError> {
        self.find_accepted_handoff(claim_id)?
            .ok_or_else(|| invalid("typed handoff unavailable"))
    }

    /// The claim's accepted handoff, or `None` when none was accepted, so
    /// callers can tell an absent handoff from a failed read.
    pub fn find_accepted_handoff(
        &self,
        claim_id: &str,
    ) -> Result<Option<AcceptedHandoff>, OrbitError> {
        self.coordination_row(HANDOFF, claim_id)?
            .map(|r| decode(&r.payload_json))
            .transpose()
    }

    fn artifact_bytes(
        &self,
        task_id: &str,
        reference: &HandoffArtifactRef,
    ) -> Result<Vec<u8>, OrbitError> {
        orbit_types::task::validate_relative_artifact_path(&reference.path)?;
        // Full bundle read verifies the manifest and payload digests before exposure.
        let bundle = self.bundle_store.read_bundle(task_id)?;
        let file = bundle
            .artifact_manifest
            .as_ref()
            .and_then(|m| m.files.iter().find(|f| f.path == reference.path))
            .ok_or_else(|| invalid("owner validation artifact missing"))?;
        if file.sha256 != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        let bytes = std::fs::read(
            self.bundle_store
                .bundle_path(task_id)?
                .join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME)
                .join(&file.blob),
        )?;
        if format!("{:x}", Sha256::digest(&bytes)) != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        Ok(bytes)
    }

    fn validate_handoff_evidence(
        &self,
        handoff: &TaskHandoff,
        required: &[String],
    ) -> Result<(), OrbitError> {
        let commands: BTreeSet<_> = required.iter().map(String::as_str).collect();
        if commands.is_empty()
            || commands.iter().any(|c| c.trim().is_empty())
            || commands.len() != required.len()
        {
            return Err(invalid(
                "owner validation requirements missing or duplicated",
            ));
        }
        let mut passed = BTreeSet::new();
        for reference in &handoff.validation {
            let log: HandoffValidationLog =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                    .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
            if log.schema_version != 1
                || log.workspace_id != handoff.workspace_id
                || log.task_id != handoff.task_id
                || log.claim_id != handoff.claim_id
                || log.machine_id != handoff.machine_id
                || log.run_id != handoff.run_id
                || log.tested_head != handoff.candidate.candidate.commit
                || log.candidate != handoff.candidate
                || log.exit_code != 0
                || log.command.trim().is_empty()
                || !passed.insert(log.command)
            {
                return Err(invalid(
                    "validation must pass for the exact claim/run/candidate/base",
                ));
            }
        }
        if !commands.iter().all(|c| passed.contains(*c)) {
            return Err(invalid("required validation missing"));
        }
        if let HandoffDelivery::AlreadyLanded {
            covering_commit,
            evidence,
        } = &handoff.candidate.delivery
        {
            let proof: AlreadyLandedEvidence =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, evidence)?)
                    .map_err(|e| invalid(&format!("invalid already-landed evidence: {e}")))?;
            let bundle = self
                .bundle_store
                .read_bundle_lightweight(&handoff.task_id)?;
            let comments = bundle
                .comments
                .iter()
                .map(|c| orbit_types::task::TaskComment {
                    at: c.at,
                    by: c.by.clone(),
                    message: c.body.clone(),
                })
                .collect::<Vec<_>>();
            let task = crate::repository::task::TaskV2Store::new(
                self.registry.clone(),
                self.workspace_id.clone(),
            )
            .task_from_bundle(bundle)?;
            if proof.schema_version != 1
                || proof.task_id != handoff.task_id
                || proof.covering_task_id != handoff.task_id
                || proof.run_id != handoff.run_id
                || proof.tested_head != handoff.candidate.candidate.commit
                || proof.covering_commit != *covering_commit
                || !object_id(covering_commit)
                || handoff.candidate.candidate != handoff.candidate.base
                || proof.scope != already_landed_scope(&task, &comments)
                || proof.required_commands != required
                || proof.validation.len() != required.len()
                || task.acceptance_criteria.is_empty()
                || proof.criteria_evidence.len() != task.acceptance_criteria.len()
                || proof.criteria_evidence.iter().any(|s| s.trim().is_empty())
            {
                return Err(invalid(
                    "already-landed evidence identity, scope or criteria mismatch",
                ));
            }
            let mut checked = BTreeSet::new();
            for check in &proof.validation {
                if check.validation.outcome != orbit_types::workflow::ValidationOutcome::Passed
                    || check.validation.role != orbit_types::workflow::ValidationRole::Required
                    || !commands.contains(check.validation.command.as_str())
                    || !checked.insert(&check.validation.command)
                {
                    return Err(invalid("already-landed validation incomplete"));
                }
                let reference = handoff
                    .validation
                    .iter()
                    .find(|r| r.path == check.log_artifact)
                    .ok_or_else(|| invalid("already-landed validation log missing"))?;
                let log: HandoffValidationLog =
                    serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                        .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
                if log.command != check.validation.command {
                    return Err(invalid("already-landed validation log mismatch"));
                }
            }
        }
        Ok(())
    }

    fn observe_handoff<'a>(
        &self,
        auth: &'a ClaimInvocation,
        candidate: &HandoffCandidate,
    ) -> Result<&'a HandoffObservation, OrbitError> {
        let observation = auth
            .handoff_observation
            .as_ref()
            .ok_or_else(|| invalid("trusted owner candidate observation required"))?;
        if observation.candidate != *candidate {
            return Err(invalid("candidate or delivery identity changed"));
        }
        Ok(observation)
    }

    pub(super) fn accept_typed_handoff(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff: &TaskHandoff,
        params: &mut TaskCoordinationCommitParams,
    ) -> Result<(), OrbitError> {
        let observation = self.observe_handoff(auth, &handoff.candidate)?;
        let bound = state
            .bound_run
            .as_ref()
            .ok_or_else(|| invalid("bound run required"))?;
        if auth.operator
            || state.claim.phase != ExecutionClaimPhase::Running
            || handoff.schema_version != 1
            || handoff.workspace_id != self.workspace_id
            || handoff.task_id != auth.task_id
            || handoff.claim_id != auth.claim_id
            || handoff.machine_id != bound.machine_id
            || handoff.run_id != bound.run_id
            || handoff.review.policy != ReviewTiming::None
            || handoff.execution_summary.trim().is_empty()
            || handoff
                .execution_summary
                .lines()
                .find(|s| !s.trim().is_empty())
                .map(str::trim)
                == Some("Outcome: failed")
        {
            return Err(invalid(
                "invalid typed handoff identity, review policy or summary",
            ));
        }
        let candidate = &handoff.candidate;
        if [
            &candidate.repository,
            &candidate.source_branch,
            &candidate.base_branch,
            &candidate.landing_branch,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
            || [
                &candidate.candidate.commit,
                &candidate.candidate.tree,
                &candidate.base.commit,
                &candidate.base.tree,
            ]
            .iter()
            .any(|s| !object_id(s))
        {
            return Err(invalid(
                "exact repository, branches and object IDs required",
            ));
        }
        let AdmissionLookup::Found { receipt, .. } = self.lookup_admission(
            &AdmissionIdentity::trusted_local(state.claim.executed_on.clone()),
            &state.claim.request_id,
        )?
        else {
            return Err(invalid("claim receipt unavailable"));
        };
        let ship = receipt.request.ship;
        if ship.review_policy != "none"
            || receipt.request.caller_review_policy != "none"
            || candidate.base_branch != ship.base_branch
            || candidate.landing_branch != ship.landing_branch
            || !matches!(
                (&candidate.delivery, ship.mode.as_str()),
                (HandoffDelivery::PullRequest { number: 1.. }, "pr")
                    | (HandoffDelivery::LocalCandidate, "local")
                    | (HandoffDelivery::AlreadyLanded { .. }, "pr" | "local")
            )
        {
            return Err(invalid("handoff differs from captured ship contract"));
        }
        self.validate_handoff_evidence(handoff, &observation.required_commands)?;
        let accepted = AcceptedHandoff {
            handoff_id: format!("handoff-{:x}", Sha256::digest(encode(handoff)?)),
            handoff: handoff.clone(),
            required_commands: observation.required_commands.clone(),
            accepted_at: Utc::now(),
        };
        // Managed completion was authorized by an operation-mode grant; with
        // grants removed [ORB-12772] nothing can authorize a `done` contract,
        // so it is refused before the handoff row exists rather than being
        // silently downgraded to a review handoff the worker did not request.
        if ship.completion == "done" {
            return Err(invalid("managed completion unsupported"));
        }
        params.rows.push(row(HANDOFF, &auth.claim_id, &accepted)?);
        Ok(())
    }

    fn add_handoff_authorization(
        &self,
        accepted: &AcceptedHandoff,
        approver: &str,
        source: HandoffAuthorizationSource,
        params: &mut TaskCoordinationCommitParams,
    ) -> Result<(), OrbitError> {
        let id = &accepted.handoff_id;
        if self
            .coordination_rows(AUTHORIZATION)?
            .iter()
            .any(|r| &r.row_id == id)
        {
            return Ok(()); // One immutable authorization and one start per exact handoff.
        }
        let authorization = HandoffAuthorization {
            authorization_id: format!("authorization-{id}"),
            handoff_id: id.clone(),
            workspace_id: self.workspace_id.clone(),
            task_id: accepted.handoff.task_id.clone(),
            claim_id: accepted.handoff.claim_id.clone(),
            candidate: accepted.handoff.candidate.clone(),
            approver: approver.into(),
            created_at: Utc::now(),
            source,
        };
        let start = LandingStartRequest {
            handoff_id: id.clone(),
            authorization_id: authorization.authorization_id.clone(),
            state: LandingStartState::Pending,
            created_at: authorization.created_at,
        };
        params.rows.push(row(AUTHORIZATION, id, &authorization)?);
        params.rows.push(row(START, id, &start)?);
        Ok(())
    }

    pub(super) fn approve_typed_handoff(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        candidate: &HandoffCandidate,
        params: &mut TaskCoordinationCommitParams,
    ) -> Result<(), OrbitError> {
        if !auth.operator
            || state.claim.phase != ExecutionClaimPhase::HandedOff
            || state.landing_invalidated
        {
            return Err(invalid("current operator approval capability required"));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        if accepted.handoff_id != handoff_id || accepted.handoff.candidate != *candidate {
            return Err(invalid("handoff candidate mismatch"));
        }
        let observed = self.observe_handoff(auth, candidate)?;
        if observed.required_commands != accepted.required_commands {
            return Err(invalid("validation requirements changed"));
        }
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands)?;
        self.add_handoff_authorization(
            &accepted,
            &auth.machine_id,
            HandoffAuthorizationSource::Operator,
            params,
        )
    }

    pub(super) fn revoke_handoff_authority(
        &self,
        auth: &ClaimInvocation,
        reason: &str,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if !self
            .coordination_rows(HANDOFF)?
            .iter()
            .any(|r| r.row_id == auth.claim_id)
        {
            return Ok(());
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let id = &accepted.handoff_id;
        let Some(old) = self.coordination_row(START, id)? else {
            return Ok(());
        };
        let mut start: LandingStartRequest = decode(&old.payload_json)?;
        match start.state {
            LandingStartState::Revoked => return Ok(()),
            LandingStartState::Completed => {
                return Err(invalid("handoff has already landed"));
            }
            LandingStartState::Pending => {}
        }
        let revocation = HandoffRevocation {
            authorization_id: start.authorization_id.clone(),
            actor: auth.machine_id.clone(),
            reason: reason.into(),
            revoked_at: Utc::now(),
        };
        params.rows.push(row(REVOCATION, id, &revocation)?);
        start.state = LandingStartState::Revoked;
        effects.replacements.push((old, row(START, id, &start)?));
        Ok(())
    }

    /// The unrevoked completion authorization scoped to exactly this handoff
    /// and candidate. Every landing decision re-reads it; a historical row is
    /// never permission on its own.
    pub(super) fn current_landing_authorization(
        &self,
        accepted: &AcceptedHandoff,
    ) -> Result<HandoffAuthorization, OrbitError> {
        let raw = self
            .coordination_row(AUTHORIZATION, &accepted.handoff_id)?
            .ok_or_else(|| invalid("handoff awaits completion approval"))?;
        let authorization: HandoffAuthorization = decode(&raw.payload_json)?;
        if authorization.handoff_id != accepted.handoff_id
            || authorization.candidate != accepted.handoff.candidate
        {
            return Err(invalid("completion authorization scope mismatch"));
        }
        if self
            .coordination_rows(REVOCATION)?
            .iter()
            .any(|r| r.row_id == accepted.handoff_id)
        {
            return Err(invalid("landing authority revoked"));
        }
        Ok(authorization)
    }

    /// Move a pending outbox request to its settled state. Only a pending
    /// request settles: completion cannot resurrect a revoked request, and
    /// revocation cannot reopen a completed one.
    pub(super) fn settle_landing_request(
        &self,
        handoff_id: &str,
        state: LandingStartState,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        let Some(old) = self.coordination_row(START, handoff_id)? else {
            return Err(invalid("no pending landing request for this handoff"));
        };
        let mut start: LandingStartRequest = decode(&old.payload_json)?;
        if start.state != LandingStartState::Pending {
            return Err(invalid("landing request is no longer pending"));
        }
        start.state = state;
        effects
            .replacements
            .push((old, row(START, handoff_id, &start)?));
        Ok(())
    }

    /// Must be rechecked when writing merge intent. Its return value alone never
    /// authorizes a future external action; the dependent consumer owns that protocol.
    pub(super) fn check_handoff_landing(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
    ) -> Result<(), OrbitError> {
        if !auth.operator || state.landing_invalidated {
            return Err(invalid("landing authority revoked"));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let observation = self.observe_handoff(auth, &accepted.handoff.candidate)?;
        if observation.required_commands != accepted.required_commands {
            return Err(invalid("validation requirements changed"));
        }
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands)?;
        let authorization = self.current_landing_authorization(&accepted)?;
        if authorization.workspace_id != self.workspace_id
            || authorization.task_id != auth.task_id
            || authorization.claim_id != auth.claim_id
        {
            return Err(invalid("completion authorization scope mismatch"));
        }
        // A grant-sourced authorization persisted before operation mode was
        // removed can no longer be rechecked, so it fails closed.
        if let HandoffAuthorizationSource::Grant { .. } = authorization.source {
            return Err(invalid("managed completion unsupported"));
        }
        Ok(())
    }
}

fn object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}
