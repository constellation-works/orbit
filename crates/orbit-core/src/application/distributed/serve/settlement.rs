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

/// The crew the settlement itself reports: its evidence's own crew first,
/// then the typed failure's or the unusable provider's, mirroring the
/// settlement record's priority after what the follower reported beside it
/// [ORB-19602]. A handoff carries no evidence, so this is `None` for one.
fn settlement_reported_crew(settlement: &ClaimMutation) -> Option<String> {
    let named = |value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
    match settlement {
        ClaimMutation::Fail(evidence) | ClaimMutation::Release(evidence) => named(&evidence.crew)
            .or_else(|| {
                evidence
                    .failure
                    .as_ref()
                    .and_then(|failure| named(&failure.crew))
            })
            .or_else(|| {
                evidence
                    .provider_unavailable
                    .as_ref()
                    .and_then(|unavailable| named(&unavailable.crew))
            }),
        _ => None,
    }
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
        // them, and nothing that decides the settlement reads them. Once the
        // settlement stands, the leaf's crew is written back to the task,
        // since the leaf drew it on its executor [ORB-19416]. A failure or
        // release whose top-level attribution is missing still reports the
        // crew inside its evidence [ORB-19602]; a handoff carries none.
        let leaf_crew = attribution
            .crew
            .clone()
            .filter(|crew| !crew.trim().is_empty())
            .or_else(|| settlement_reported_crew(&settlement));
        #[cfg(feature = "test-support")]
        crate::application::distributed::admission_checkpoint::notify_settle();
        // Fence the post-receipt write-back against replay [ORB-19486,
        // ORB-19521]: only the call whose mutation committed writes the crew.
        // A replay, including one that read the claim unsettled before
        // another call's commit, returns the original receipt with
        // `committed: false`. Attribution is not part of the receipt
        // identity, so it must not become new mutation authority on replay.
        // Snapshot the task crew before the mutation so a delayed write also
        // cannot overwrite a crew recorded after this snapshot.
        let recorded_crew = self
            .get_task(&claim.task_id)
            .ok()
            .and_then(|task| task.crew);
        let context = ClaimInvocation::trusted_worker(
            claim.task_id.clone(),
            claim.claim_id.clone(),
            machine,
            run,
        )
        .with_attribution(attribution);
        let outcome = match settlement {
            ClaimMutation::Fail(evidence) => {
                self.validate_claimed_review_carry(&claim, evidence.claimed_review.as_ref())?;
                let evidence = self.prepare_claimed_review_settlement(&claim, evidence);
                let final_recovery = evidence.final_recovery.clone();
                let carry = evidence.claimed_review.clone();
                let outcome = self.mutate_execution_claim_outcome(
                    Some(&context),
                    &fail_mutation_id(&claim.claim_id),
                    &ClaimMutation::Fail(evidence),
                )?;
                self.import_claimed_review_ledgers(carry.as_ref());
                if let Some(final_recovery) = final_recovery {
                    self.apply_settled_final_recovery(
                        &claim,
                        run_id,
                        &outcome.result,
                        &final_recovery,
                    );
                }
                Ok(outcome)
            }
            ClaimMutation::Release(evidence) => {
                self.validate_claimed_review_carry(&claim, evidence.claimed_review.as_ref())?;
                let carry = evidence.claimed_review.clone();
                let context = match self.refused_release_hold(&claim, run_id, &evidence) {
                    Some(hold) => context.with_provider_hold(hold),
                    None => context,
                };
                let outcome = self.mutate_execution_claim_outcome(
                    Some(&context),
                    &release_mutation_id(&claim.claim_id),
                    &ClaimMutation::Release(evidence),
                )?;
                self.import_claimed_review_ledgers(carry.as_ref());
                Ok(outcome)
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
                self.accept_task_handoff_outcome(
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
        }?;
        self.write_back_claimed_leaf_crew(
            &claim.task_id,
            leaf_crew.as_deref(),
            outcome.committed,
            recorded_crew.as_deref(),
        );
        Ok(outcome.result)
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
