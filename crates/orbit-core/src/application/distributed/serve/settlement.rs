use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimAttribution, ClaimEvidence, ClaimInvocation, ClaimMutation, ClaimMutationResult, ClaimRun,
    ExecutionClaim,
};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{ClaimFailureClass, ProviderFailureClass, ProviderFailureHold};

use crate::OrbitRuntime;
use crate::application::distributed::contract::is_remote;
use crate::application::distributed::ensure_distributed_mutation_available;

use super::refusal;

fn fail_mutation_id(claim_id: &str) -> String {
    format!("pull-fail:{claim_id}")
}

pub(super) fn release_mutation_id(claim_id: &str) -> String {
    format!("pull-release:{claim_id}")
}

fn handoff_mutation_id(claim_id: &str) -> String {
    format!("pull-handoff:{claim_id}")
}

impl OrbitRuntime {
    /// Settle a claim with the follower's durable settlement: a typed handoff
    /// or a failure. Anything else is a lifecycle operation the executor does
    /// not own — approval, revocation and recovery stay owner-operator acts.
    ///
    /// A failure may carry the leaf's final-recovery decision [ORB-13907]; the
    /// owner applies it to its own task once the claim has failed.
    pub fn serve_claim_settle(
        &self,
        session: &ToolSessionContext,
        claim_id: &str,
        run_id: Option<&str>,
        settlement: ClaimMutation,
        attribution: ClaimAttribution,
    ) -> Result<ClaimMutationResult, OrbitError> {
        ensure_distributed_mutation_available("orbit.drain.claim.settle")?;
        self.ensure_distributed_owner_workspace()?;
        let machine = self.session_caller_machine(session)?;
        let claim = self.current_claim(claim_id)?;
        let run = run_id
            .map(str::trim)
            .filter(|run| !run.is_empty())
            .map(|run_id| ClaimRun {
                machine_id: machine.clone(),
                run_id: run_id.to_string(),
            });
        // The leaf's crew and model are attribution only: the record keeps
        // them, and nothing that decides the settlement reads them.
        let context = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            machine,
            run,
        )
        .with_attribution(attribution);
        match settlement {
            ClaimMutation::Fail(evidence) => {
                self.validate_claimed_review_carry(&claim, evidence.claimed_review.as_ref())?;
                let evidence = self.prepare_claimed_review_settlement(&claim, evidence);
                let final_recovery = evidence.final_recovery.clone();
                let carry = evidence.claimed_review.clone();
                let result = self.mutate_execution_claim(
                    Some(&context),
                    &fail_mutation_id(&claim.claim_id),
                    &ClaimMutation::Fail(evidence),
                )?;
                self.import_claimed_review_ledgers(carry.as_ref());
                if let Some(final_recovery) = final_recovery {
                    self.apply_settled_final_recovery(&claim, run_id, &result, &final_recovery);
                }
                Ok(result)
            }
            ClaimMutation::Release(evidence) => {
                self.validate_claimed_review_carry(&claim, evidence.claimed_review.as_ref())?;
                let carry = evidence.claimed_review.clone();
                let context = match self.refused_release_hold(&claim, run_id, &evidence) {
                    Some(hold) => context.with_provider_hold(hold),
                    None => context,
                };
                let result = self.mutate_execution_claim(
                    Some(&context),
                    &release_mutation_id(&claim.claim_id),
                    &ClaimMutation::Release(evidence),
                )?;
                self.import_claimed_review_ledgers(carry.as_ref());
                Ok(result)
            }
            ClaimMutation::AcceptHandoff(handoff) => {
                // Recovery ends a claim that never accepted this immutable
                // handoff, even though the handoff may now name a stale PR head.
                // Refuse before any forge observation, so the follower can
                // reconcile and close its pending record. An accepted handoff
                // still reaches the journal, which replays its recorded outcome.
                if !claim.phase.is_unsettled()
                    && self
                        .stores()
                        .tasks()
                        .find_accepted_handoff(&claim.claim_id)?
                        .is_none()
                {
                    return Err(refusal::refused(
                        "stale_claim: the owner already ended this claim; its pending handoff is obsolete",
                    ));
                }
                let deadline = std::time::Instant::now() + self.claim_known_red_budget;
                let mut observation = self.observe_claim_handoff(&handoff, is_remote(session))?;
                let known_red = self.observe_known_red(&handoff, &observation, deadline)?;
                if !known_red.contradicted.is_empty() {
                    return Err(self.release_contradicted_known_red(
                        &claim,
                        &context,
                        &known_red.contradicted,
                    ));
                }
                observation.known_red = known_red.observations;
                self.accept_task_handoff(
                    &context,
                    &handoff_mutation_id(&claim.claim_id),
                    handoff,
                    observation,
                )
            }
            _ => Err(refusal::refused(
                "only a typed handoff, a failure or a release settles a claimed leaf; approval, \
                 revocation and recovery are owner-operator actions",
            )),
        }
    }

    /// [ORB-15573] The hold a release whose leaf its provider refused places
    /// on the task: every crew of the refusing provider this owner configures
    /// is excluded, as a local run's refusal excludes them. The owner builds
    /// it from its own crews and the task's history, never from the payload.
    /// A hold that cannot be built is logged and the task released unheld.
    fn refused_release_hold(
        &self,
        claim: &ExecutionClaim,
        run_id: Option<&str>,
        evidence: &ClaimEvidence,
    ) -> Option<ProviderFailureHold> {
        let failure = evidence
            .failure
            .as_ref()
            .filter(|failure| failure.class == ClaimFailureClass::Provider)
            .filter(|failure| failure.provider_refusal)?;
        let hold = self.get_task(&claim.task_id).and_then(|task| {
            self.claimed_provider_failure_hold(
                &task,
                run_id.unwrap_or(&claim.claim_id),
                failure.crew.as_deref(),
                ProviderFailureClass::Refusal,
                Some(&failure.reason),
                Utc::now(),
            )
        });
        hold.map_err(|error| {
            tracing::warn!(
                task_id = %claim.task_id,
                "could not build the provider hold for a refused claimed leaf: {error}"
            );
        })
        .ok()
    }
}
