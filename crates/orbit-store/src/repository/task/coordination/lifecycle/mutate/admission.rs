use super::super::super::TaskCommitBoundary;
use super::super::codec::RECEIPT;
use super::super::{encode, invalid};
use crate::contracts::*;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;

impl TaskCommitBoundary {
    /// Check native/landing authority and reserve a base update through the
    /// claim journal, never the generic task writer fenced by this live claim.
    pub fn reserve_handoff_base_update(
        &self,
        auth: &ClaimInvocation,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        use orbit_types::workflow::{PrBaseUpdateAdmission, decide_pr_base_update};
        self.with_admission(|| {
            let authority = self.claim_authority(auth, false)?;
            self.check_base_update_scope(auth, &authority.state, request)?;
            let history =
                crate::repository::task::v2::task_history_from_events(authority.bundle.events);
            let decision = decide_pr_base_update(&history, request);
            if matches!(decision, PrBaseUpdateAdmission::Reserved) {
                let mutation = ClaimMutation::ReserveBaseUpdate(request.clone());
                let mutation_id =
                    format!("base-update:{}:{}", request.repository, request.pr_number);
                let receipt_id = sha256_hex(encode(&(&auth.claim_id, &mutation_id))?.as_bytes());
                let input = encode(&(
                    &auth.task_id,
                    &auth.claim_id,
                    &auth.machine_id,
                    &auth.run,
                    auth.operator,
                    &mutation,
                ))?;
                self.mutate_claim_locked(auth, &mutation, &receipt_id, input)?;
            }
            Ok(decision)
        })
    }

    pub(super) fn check_base_update_scope(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<(), OrbitError> {
        if state.claim.phase != ExecutionClaimPhase::HandedOff {
            return Err(invalid("current handed-off landing claim required"));
        }
        self.check_handoff_landing(auth, state)?;
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let candidate = &accepted.handoff.candidate;
        if request.execution != accepted.handoff_id
            || request.repository != candidate.repository
            || request.head_sha != candidate.candidate.commit
            || request.head_branch != candidate.source_branch
            || request.base_branch != candidate.base_branch
            || candidate.delivery
                != (orbit_types::workflow::handoff::HandoffDelivery::PullRequest {
                    number: request
                        .pr_number
                        .parse()
                        .map_err(|_| invalid("base update PR identity required"))?,
                })
        {
            return Err(invalid("base update differs from accepted handoff"));
        }
        Ok(())
    }

    /// Trusted-context seam for application callers and the later generic tool adapter.
    /// Missing invocation context is always refused, even for an unclaimed task.
    pub fn mutate_execution_claim(
        &self,
        invocation: Option<&ClaimInvocation>,
        mutation_id: &str,
        mutation: &ClaimMutation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let auth = invocation.ok_or_else(|| invalid("claim invocation context required"))?;
        if [mutation_id, &auth.task_id, &auth.claim_id, &auth.machine_id]
            .iter()
            .any(|s| s.trim().is_empty())
        {
            return Err(invalid("invalid claim invocation"));
        }
        let mut identity = mutation.clone();
        if let ClaimMutation::Friction(params) = &mut identity {
            params.created_at = chrono::DateTime::UNIX_EPOCH;
        }
        let input = encode(&(
            &auth.task_id,
            &auth.claim_id,
            &auth.machine_id,
            &auth.run,
            auth.operator,
            &identity,
        ))?;
        let receipt_id = sha256_hex(encode(&(&auth.claim_id, mutation_id))?.as_bytes());
        // A retry of a recorded mutation only reads, so it replays in an
        // ordinary section instead of excluding the partition's readers. The
        // peek outside the boundary only picks the route: the replay reads
        // the receipt again once any pending commit is settled, and a
        // mutation whose receipt lands after the peek finds it inside the
        // admission section.
        let recorded = self
            .store
            .task_coordination_row(&self.workspace_id, RECEIPT, &receipt_id)?
            .is_some();
        let replayed = if recorded {
            self.enter_ordinary(|| self.replay_claim_receipt(auth, mutation, &receipt_id, &input))?
        } else {
            None
        };
        let result = match replayed {
            Some(result) => result,
            None => self
                .with_admission(|| self.mutate_claim_locked(auth, mutation, &receipt_id, input))?,
        };
        // Each settlement sweeps receipts past retention, outside the section.
        if !result.phase.is_unsettled() {
            self.prune_settled_receipts_after_settlement();
        }
        Ok(result)
    }
}
