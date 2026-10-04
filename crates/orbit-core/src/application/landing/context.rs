//! The owner state a landing activity runs against, and the steps it records.

use orbit_common::OrbitError;
use orbit_engine::{HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate};
use orbit_store::contracts::{ClaimMutation, HandoffObservation};
use orbit_types::workflow::handoff::AcceptedHandoff;

use super::attempts::missing_attempt;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// The owner state one landing attempt runs against.
    ///
    /// Everything here is owner-held: the accepted candidate identity and the
    /// merge intent this owner has not resolved. The activity treats none of it
    /// as evidence that anything merged.
    pub(crate) fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let (claim, accepted) = self.landing_claim(handoff_id)?;
        Ok(HandoffLandingContext {
            handoff_id: accepted.handoff_id,
            task_id: accepted.handoff.task_id,
            claim_id: accepted.handoff.claim_id,
            candidate: accepted.handoff.candidate,
            unresolved_merge_intent: claim.unresolved_merge_intent,
            workspace_path: self.paths().repo_root.clone(),
        })
    }

    /// Record one landing step from the activity that observed it.
    ///
    /// The observation is accepted only when it matches the accepted candidate
    /// exactly; a changed repository, branch, head or base refuses here instead
    /// of reaching an external merge. The required validation commands are the
    /// owner's own, captured at acceptance — never anything the step supplied.
    pub(crate) fn record_handoff_landing(
        &self,
        update: &HandoffLandingUpdate,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let (claim, accepted) = self.landing_claim(&update.handoff_id)?;
        let mut context = self.landing_invocation(&claim);
        if matches!(
            update.step,
            HandoffLandingStep::PublishIntent { .. } | HandoffLandingStep::Complete
        ) {
            context =
                context.with_handoff_observation(self.landing_observation(update, &accepted)?);
        }
        let attempt = self
            .landing_attempt(&update.handoff_id)?
            .ok_or_else(|| missing_attempt(&update.handoff_id))?
            .attempt;
        // Every landing mutation id is scoped to the attempt that performs it.
        // The store refuses a replayed unresolved merge intent outright — a
        // persisted send intent is uncertainty, never permission to send again —
        // so an id held stable across attempts would let one failed merge
        // wedge the handoff forever: the next attempt reconciles the intent,
        // then can never publish a fresh one. Scoping keeps that refusal exactly
        // where it belongs, inside the attempt that published.
        let (mutation_id, mutation) = match &update.step {
            HandoffLandingStep::PublishIntent { intent_id } => (
                format!("landing-intent:{intent_id}:{attempt}"),
                ClaimMutation::MergeIntent {
                    intent_id: intent_id.clone(),
                    resolved: false,
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::ResolveIntent { intent_id, merged } => (
                format!("landing-resolve:{intent_id}:{merged}:{attempt}"),
                ClaimMutation::MergeIntent {
                    intent_id: intent_id.clone(),
                    resolved: true,
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::Complete => (
                format!("landing-complete:{}:{attempt}", update.handoff_id),
                ClaimMutation::CompleteLanding {
                    handoff_id: update.handoff_id.clone(),
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::Stop => (
                format!("landing-stop:{}:{attempt}", update.handoff_id),
                ClaimMutation::StopLanding {
                    handoff_id: update.handoff_id.clone(),
                    reason: update.evidence.clone(),
                },
            ),
        };
        self.stores()
            .tasks()
            .mutate_execution_claim(Some(&context), &mutation_id, &mutation)?;
        Ok(())
    }

    fn landing_observation(
        &self,
        update: &HandoffLandingUpdate,
        accepted: &AcceptedHandoff,
    ) -> Result<HandoffObservation, OrbitError> {
        let observed = update.observed.as_ref().ok_or_else(|| {
            OrbitError::InvalidInput(
                "landing authority decisions require an owner candidate observation".to_string(),
            )
        })?;
        if *observed != accepted.handoff.candidate {
            return Err(OrbitError::InvalidInput(format!(
                "observed candidate for handoff '{}' is not the accepted one; landing stops \
                 until a fresh validated handoff replaces it",
                update.handoff_id
            )));
        }
        Ok(HandoffObservation {
            footprint_widening: accepted.handoff.footprint_widening.clone(),
            candidate: accepted.handoff.candidate.clone(),
            required_commands: accepted.required_commands.clone(),
            owner_completion_authority: self.owner_completion_authority(),
        })
    }
}
