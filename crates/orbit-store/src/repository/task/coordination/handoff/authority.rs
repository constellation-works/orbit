//! Completion authorization, revocation and landing checks.
use chrono::Utc;
use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::workflow::handoff::*;

use super::super::lifecycle::{decode, invalid, row};
use super::{AUTHORIZATION, HANDOFF, REVOCATION, START, TaskCommitBoundary};
use crate::contracts::*;

impl TaskCommitBoundary {
    pub(super) fn add_handoff_authorization(
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

    pub(in super::super) fn approve_typed_handoff(
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
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::HandoffCandidateMismatch,
            ));
        }
        let observed = self.observe_handoff(auth, candidate)?;
        if observed.required_commands != accepted.required_commands {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::ValidationRequirementsChanged,
            ));
        }
        self.validate_handoff_review(
            &accepted.handoff,
            &self.claim_ship(state)?,
            observed.review.as_ref(),
            false,
        )?;
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands, None)?;
        self.add_handoff_authorization(
            &accepted,
            &auth.machine_id,
            HandoffAuthorizationSource::Operator,
            params,
        )
    }

    pub(in super::super) fn revoke_handoff_authority(
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
                return Err(OrbitError::claim_refused(
                    ClaimRefusalKind::HandoffAlreadyLanded,
                ));
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
    pub(in super::super) fn current_landing_authorization(
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
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::LandingAuthorityRevoked,
            ));
        }
        Ok(authorization)
    }

    /// Move a pending outbox request to its settled state. Only a pending
    /// request settles: completion cannot resurrect a revoked request, and
    /// revocation cannot reopen a completed one.
    pub(in super::super) fn settle_landing_request(
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
    pub(in super::super) fn check_handoff_landing(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
    ) -> Result<(), OrbitError> {
        if !auth.operator || state.landing_invalidated {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::LandingAuthorityRevoked,
            ));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let observation = self.observe_handoff(auth, &accepted.handoff.candidate)?;
        if observation.required_commands != accepted.required_commands {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::ValidationRequirementsChanged,
            ));
        }
        // The landing merges exactly the accepted candidate, which acceptance
        // proved is the reviewed head; recheck the pinned review evidence too.
        self.validate_handoff_review(
            &accepted.handoff,
            &self.claim_ship(state)?,
            observation.review.as_ref(),
            false,
        )?;
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands, None)?;
        let authorization = self.current_landing_authorization(&accepted)?;
        if authorization.workspace_id != self.workspace_id
            || authorization.task_id != auth.task_id
            || authorization.claim_id != auth.claim_id
        {
            return Err(invalid("completion authorization scope mismatch"));
        }
        match &authorization.source {
            HandoffAuthorizationSource::Operator => {}
            // A policy authorization lands only while the owner still grants
            // that policy; withdrawing the key stops every unlanded handoff.
            HandoffAuthorizationSource::OwnerPolicy { reference } => {
                if observation.owner_completion_authority.as_deref() != Some(reference.as_str()) {
                    return Err(invalid(
                        "owner completion policy withdrawn; restore `workflow.distributed_completion = \"done\"` \
                         or revoke this handoff",
                    ));
                }
            }
            // A grant-sourced authorization persisted before operation mode
            // was removed can no longer be rechecked, so it fails closed.
            HandoffAuthorizationSource::Grant { .. } => {
                return Err(invalid("managed completion unsupported"));
            }
        }
        Ok(())
    }
}
