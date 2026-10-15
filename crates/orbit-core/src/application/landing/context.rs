//! The owner state a landing activity runs against, and the steps it records.

use orbit_common::OrbitError;
use orbit_engine::{HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate};
use orbit_store::contracts::{ClaimMutation, HandoffObservation};
use orbit_types::workflow::handoff::AcceptedHandoff;
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::{Value, json};

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
        // [ORB-15677] The hosted checks the accepted review owed the owner,
        // read from the certificate the handoff pinned and acceptance judged.
        let certificate = self
            .admission_boundary()?
            .accepted_review_certificate(&accepted.handoff.claim_id)?;
        let owner_owed_evidence = certificate
            .as_ref()
            .map(|certificate| certificate.owner_owed_evidence.clone())
            .unwrap_or_default();
        let mut owed_hosted_checks: Vec<String> = owner_owed_evidence
            .iter()
            .filter_map(|owed| owed.hosted_check.clone())
            .collect();
        if let Some(source) = claim
            .claim
            .repair
            .as_ref()
            .and_then(|repair| repair.source.as_ref())
        {
            owed_hosted_checks.extend(
                source
                    .checks
                    .iter()
                    .map(|check| format!("{} / {}", check.workflow, check.name)),
            );
            owed_hosted_checks.sort();
            owed_hosted_checks.dedup();
        }
        let attempt = self.landing_attempt(handoff_id)?;
        let receipt = self.claim_receipt(&accepted.handoff.claim_id)?;
        Ok(HandoffLandingContext {
            handoff_id: accepted.handoff_id,
            task_id: accepted.handoff.task_id,
            claim_id: accepted.handoff.claim_id,
            candidate: accepted.handoff.candidate,
            unresolved_merge_intent: claim.unresolved_merge_intent,
            workspace_path: self.paths().repo_root.clone(),
            owed_hosted_checks,
            owner_review_certificate: certificate,
            // [ORB-15956] A repair claim has spent the one automatic repair.
            automatic_repair_spent: claim.claim.repair.is_some(),
            source_repair_authorized: claim.claim.repair_authorization.is_some()
                && receipt.request.ship.review.is_some()
                && receipt.request.ship.review_contract_consistent(),
            repair_footprint: claim.claim.footprint.clone(),
            source_repair: claim
                .claim
                .repair
                .as_ref()
                .and_then(|repair| repair.source.clone()),
            check_recovery: attempt
                .map(|attempt| attempt.check_recovery)
                .unwrap_or_default(),
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
            HandoffLandingStep::PublishIntent { .. }
                | HandoffLandingStep::Complete
                | HandoffLandingStep::SourceRepair { .. }
                | HandoffLandingStep::CheckRecovery { .. }
        ) {
            let mut observation = self.landing_observation(update, &accepted)?;
            if matches!(update.step, HandoffLandingStep::SourceRepair { .. })
                || claim
                    .claim
                    .repair
                    .as_ref()
                    .is_some_and(|repair| repair.source.is_some())
            {
                observation.required_commands =
                    self.workflow_required_validation_commands().to_vec();
            }
            context = context.with_handoff_observation(observation);
        }
        if let HandoffLandingStep::SourceRepair { source } = &update.step {
            orbit_engine::validate_landing_source_identity(
                self,
                &self.handoff_landing_context(&update.handoff_id)?,
                source,
            )?;
            context = context.with_source_repair(source.clone());
        }
        let attempt = self
            .landing_attempt(&update.handoff_id)?
            .ok_or_else(|| missing_attempt(&update.handoff_id))?
            .attempt;
        let completing = matches!(update.step, HandoffLandingStep::Complete);
        let previous_task = completing
            .then(|| self.get_task(&accepted.handoff.task_id))
            .transpose()?;
        // Retain the exact delivery identity in the durable completion note,
        // including paths with no external merge, so failed friction writes
        // can retry later without consulting mutable refs or the provider.
        let mut evidence = update.evidence.clone();
        if completing
            && let Ok(mut value @ Value::Object(_)) = serde_json::from_str::<Value>(&evidence)
        {
            let candidate = &accepted.handoff.candidate;
            let (commit, pr) = match &candidate.delivery {
                HandoffDelivery::PullRequest { number } => (
                    value
                        .pointer("/delivery/merge_commit")
                        .and_then(Value::as_str),
                    Some(*number),
                ),
                HandoffDelivery::LocalCandidate => {
                    (Some(candidate.candidate.commit.as_str()), None)
                }
                HandoffDelivery::AlreadyLanded {
                    covering_commit, ..
                } => (Some(covering_commit.as_str()), None),
                HandoffDelivery::NoDiff { .. } => (Some(candidate.base.commit.as_str()), None),
            };
            if let Some(commit) = commit {
                let resolution = json!({"commit": commit, "pr_number": pr});
                value["friction_resolution"] = resolution;
                evidence = value.to_string();
            }
        }
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
                    evidence,
                },
            ),
            HandoffLandingStep::SourceRepair { .. } => (
                format!("landing-source-stop:{}:{attempt}", update.handoff_id),
                ClaimMutation::StopSourceRepair {
                    handoff_id: update.handoff_id.clone(),
                    reason: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::CheckRecovery { recovery } => (
                format!(
                    "landing-recovery:{}:{}",
                    update.handoff_id,
                    serde_json::to_string(recovery)
                        .map_err(|error| OrbitError::Execution(error.to_string()))?
                ),
                ClaimMutation::RecordLandingRecovery {
                    handoff_id: update.handoff_id.clone(),
                    recovery: recovery.clone(),
                },
            ),
            HandoffLandingStep::Stop { repairable } => (
                format!("landing-stop:{}:{attempt}", update.handoff_id),
                ClaimMutation::StopLanding {
                    handoff_id: update.handoff_id.clone(),
                    reason: update.evidence.clone(),
                    repairable: *repairable,
                },
            ),
        };
        self.stores()
            .tasks()
            .mutate_execution_claim(Some(&context), &mutation_id, &mutation)?;
        if let Some(previous_task) = previous_task {
            let task = self.get_task(&previous_task.id)?;
            self.record_resolves_side_effects(previous_task.status, &task);
        }
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
        if matches!(
            accepted.handoff.candidate.delivery,
            orbit_types::workflow::handoff::HandoffDelivery::NoDiff { .. }
        ) {
            // Completion re-runs the same base and verifier-report checks as
            // acceptance, including current owner-required validation policy.
            return self.observe_claim_handoff(&accepted.handoff, false);
        }
        Ok(HandoffObservation {
            footprint_widening: accepted.handoff.footprint_widening.clone(),
            candidate: accepted.handoff.candidate.clone(),
            required_commands: accepted.required_commands.clone(),
            owner_completion_authority: self.owner_completion_authority(),
            // Acceptance observed the review's base and repository; the store
            // rechecks the review evidence the handoff pinned without them.
            review: None,
            // Acceptance ran the known-red bases; landing rechecks the logs.
            known_red: Vec::new(),
        })
    }
}
