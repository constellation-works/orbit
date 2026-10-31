use orbit_common::OrbitError;
use orbit_engine::BaseSyncMode;
use orbit_store::contracts::{HandoffObservation, HandoffReviewObservation};
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery, TaskHandoff};

use crate::OrbitRuntime;
use crate::application::automation::source::Source;

use super::refusal;

impl OrbitRuntime {
    /// The owner's own reading of the candidate a claim is settling.
    ///
    /// Read from the owner checkout — and, for a published delivery, from the
    /// provider — with the shared observation rules, so the worker's handoff
    /// payload contributes nothing but the identity to look *at*. The claim
    /// journal then compares this observation against the submitted candidate.
    ///
    /// `remote` refuses a local candidate: it exists only in the executor's
    /// checkout, which the owner cannot read, and followers never run local
    /// mode. NoDiff verifies a clean-tree checkpoint against the base its run
    /// synchronized onto, which the owner's live base must still contain; the
    /// executor's branch need not be published.
    pub(crate) fn observe_claim_handoff(
        &self,
        handoff: &TaskHandoff,
        remote: bool,
    ) -> Result<HandoffObservation, OrbitError> {
        self.observe_handoff_candidate(handoff, remote, None)
    }

    /// Only a stored acceptance may use provider base-only adoption. Initial
    /// settlement continues through the strict published candidate observer.
    pub(crate) fn observe_accepted_claim_handoff(
        &self,
        accepted: &orbit_types::workflow::handoff::AcceptedHandoff,
    ) -> Result<HandoffObservation, OrbitError> {
        let current = self.accepted_task_handoff(&accepted.handoff.claim_id)?;
        if current != *accepted {
            return Err(refusal::refused(
                "accepted handoff changed before observation",
            ));
        }
        self.observe_handoff_candidate(&accepted.handoff, false, Some(accepted))
    }

    fn observe_handoff_candidate(
        &self,
        handoff: &TaskHandoff,
        remote: bool,
        accepted: Option<&orbit_types::workflow::handoff::AcceptedHandoff>,
    ) -> Result<HandoffObservation, OrbitError> {
        let receipt = self.claim_receipt(&handoff.claim_id)?;
        if receipt
            .visual_review
            .as_ref()
            .is_some_and(|contract| contract.mode == orbit_types::task::VisualReviewMode::Live)
            || (self.visual_review_settings().mode == orbit_types::task::VisualReviewMode::Live
                && orbit_types::task::has_verify_mock_ui_tag(
                    &self.get_task(&handoff.task_id)?.tags,
                ))
        {
            return Err(refusal::refused(
                "visual_live_claim_unsupported: the owner cannot verify final-candidate visual resolutions for this claimed delivery",
            ));
        }
        let candidate = match handoff.candidate.delivery {
            HandoffDelivery::NoDiff { .. } => orbit_engine::observe_no_diff_candidate(
                self,
                &self.paths().repo_root,
                handoff,
                if receipt.request.ship.mode == "pr" {
                    BaseSyncMode::Remote
                } else {
                    BaseSyncMode::Local
                },
            )?,
            HandoffDelivery::LocalCandidate if remote => {
                return Err(refusal::refused(
                    "a follower cannot hand off a local candidate: followers never execute \
                     owner-local leaves, and the owner cannot observe a candidate that exists \
                     only in the executor's checkout",
                ));
            }
            HandoffDelivery::LocalCandidate => orbit_engine::observe_candidate(
                &self.paths().repo_root,
                Some(&handoff.candidate.source_branch),
                &handoff.candidate.base_branch,
                &handoff.candidate.landing_branch,
                HandoffDelivery::LocalCandidate,
                &handoff.workspace_id,
                // An owner-local candidate has no origin to fetch and must
                // keep reading the local base it was synchronized onto.
                BaseSyncMode::Local,
            )?,
            // [ORB-12500] The owner reads the published pull request itself:
            // the provider names the delivery, and the candidate and base
            // objects are resolved in this checkout.
            HandoffDelivery::PullRequest { .. } if accepted.is_some() => {
                orbit_engine::observe_accepted_published_handoff(
                    self,
                    &self.paths().repo_root,
                    accepted.ok_or_else(|| refusal::refused("accepted handoff required"))?,
                )?
            }
            HandoffDelivery::PullRequest { .. } => orbit_engine::observe_published_candidate(
                self,
                &self.paths().repo_root,
                &handoff.candidate,
            )?,
            HandoffDelivery::AlreadyLanded { .. } => {
                return Err(refusal::refused(
                    "already-landed delivery carries its own typed report through the no-diff \
                     verifier; a claimed leaf does not hand one off",
                ));
            }
        };
        let original = receipt
            .claim
            .as_ref()
            .ok_or_else(|| refusal::refused("original claim footprint unavailable"))?;
        let (new_paths, footprint_widening) = orbit_engine::validate_claim_new_paths(
            &self.paths().repo_root,
            &original.footprint,
            &candidate.base.commit,
            &candidate.candidate.commit,
        )?;
        for path in &new_paths {
            let decision = self.policy_engine().check(
                "implementer",
                orbit_types::policy::FsOperation::Modify,
                path.clone(),
            )?;
            if !decision.allowed {
                return Err(refusal::refused(format!(
                    "footprint widening refused protected path: {path}"
                )));
            }
        }
        self.verify_selective_receipt(handoff, &candidate, &receipt.request.ship)?;
        let review = self.observe_handoff_review(handoff, &candidate)?;
        let light_delivery = self.owner_light_delivery(&handoff.task_id, &candidate)?;
        // An empty list is no required check: the handoff is accepted with no
        // validation logs, the way the owner's own delivery runs none.
        Ok(HandoffObservation {
            footprint_widening,
            candidate,
            required_commands: self.workflow_required_validation_commands().to_vec(),
            owner_completion_authority: self.owner_completion_authority(),
            review,
            known_red: Vec::new(),
            light_delivery,
        })
    }

    /// Re-evaluate a claimed selective selection against the owner's own
    /// reading of the candidate diff: an agent's claimed non-match is
    /// insufficient. The receipt's frozen model coefficients decide the
    /// risk predicate, so owner and leaf judge the same model.
    fn verify_selective_receipt(
        &self,
        handoff: &TaskHandoff,
        candidate: &HandoffCandidate,
        ship: &orbit_store::contracts::AdmissionShipContract,
    ) -> Result<(), OrbitError> {
        use orbit_types::workflow::{SelectiveReviewResult, selective_path_matches};
        let Some(policy) = ship.selective_policy() else {
            return Ok(());
        };
        if matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }) {
            return Ok(());
        }
        let Some(receipt) = &handoff.review.selective_receipt else {
            return Err(refusal::refused(
                "the claim captured an active selective before-landing policy; the handoff \
                 carries no selection receipt",
            ));
        };
        if policy.risk_threshold.is_some()
            && (policy.risk_model_identity.is_none()
                || receipt.risk_model_identity != policy.risk_model_identity)
        {
            return Err(refusal::refused(
                "selection_unavailable: receipt risk model is not the frozen owner snapshot",
            ));
        }
        // Selection describes the original candidate. A stronger review may
        // certify repairs at a later head; Store verifies that certificate's
        // original/final linkage before acceptance changes any task state.
        let selected = orbit_engine::review_gate::candidate_identity_at(
            &self.paths().repo_root,
            &receipt.base_sha,
            &receipt.head_sha,
        )
        .map_err(|error| {
            refusal::refused(format!(
                "selection_unavailable: owner cannot read the selected candidate identity: {error}"
            ))
        })?;
        if selected.base.commit != candidate.base.commit
            || selected.base.commit != receipt.base_sha
            || selected.base.tree != receipt.base_tree
            || selected.head.commit != receipt.head_sha
            || selected.head.tree != receipt.head_tree
            || (selected.head != candidate.candidate && handoff.review.evidence().is_none())
        {
            return Err(refusal::refused(
                "selection_stale: receipt does not pin the original candidate or carry review evidence for its continuation",
            ));
        }
        // Generic candidate/Git/footprint proof already succeeded above.
        // Observe-mode predicate gaps and disagreements are telemetry only;
        // they cannot add a delivery gate. Identity/certificate checks remain.
        if policy.mode == orbit_types::workflow::SelectiveBeforeLandingMode::Observe {
            return Ok(());
        }
        let pairs = orbit_engine::review_gate::changed_path_pairs(
            &self.paths().repo_root,
            &selected.base.commit,
            &selected.head.commit,
        )
        .map_err(|error| {
            refusal::refused(format!(
                "selection_unavailable: owner cannot read the candidate path diff: {error}"
            ))
        })?;
        if pairs
            .iter()
            .flat_map(|(old, new)| old.iter().chain(new.iter()))
            .any(|path| orbit_types::policy::normalize_glob_path(path).is_err())
        {
            return Err(refusal::refused(
                "selection_unavailable: owner cannot normalize the complete candidate path diff",
            ));
        }
        if receipt.result == SelectiveReviewResult::NotRequired {
            for (old, new) in &pairs {
                for path in old.iter().chain(new.iter()) {
                    if policy
                        .paths
                        .iter()
                        .any(|pattern| selective_path_matches(pattern, path))
                    {
                        return Err(refusal::refused(format!(
                            "selective receipt contradicted: owner-measured diff path \
                             `{path}` matches the captured selective paths, but the \
                             receipt records not_required"
                        )));
                    }
                }
            }
            if let (Some(threshold), Some(identity)) = (
                policy.risk_threshold_value(),
                policy.risk_model_identity.as_deref(),
            ) {
                let coefficient = |name: &str| {
                    identity
                        .split_whitespace()
                        .find_map(|field| field.strip_prefix(name))
                        .and_then(|value| value.parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                };
                let unknown = identity == "owner-risk-model/unknown"
                    || identity.split_whitespace().any(|field| field == "fit=none")
                    || receipt.task_ids.len() != 1;
                if unknown
                    && policy.unknown_risk == orbit_types::workflow::SelectiveUnknownRisk::Review
                {
                    return Err(refusal::refused(
                        "selective receipt contradicted: frozen owner risk is unknown and captured unknown_risk requires review",
                    ));
                }
                if !unknown
                    && (coefficient("intercept=").is_none() || coefficient("slope=").is_none())
                {
                    return Err(refusal::refused(
                        "selection_unavailable: malformed frozen owner model",
                    ));
                }
                if let (Some(intercept), Some(slope)) =
                    (coefficient("intercept="), coefficient("slope="))
                {
                    let lines = orbit_engine::review_gate::diff_lines_between(
                        &self.paths().repo_root,
                        &selected.base.commit,
                        &selected.head.commit,
                    )
                    .map_err(|error| {
                        refusal::refused(format!(
                            "selection_unavailable: owner cannot measure the candidate \
                             diff: {error}"
                        ))
                    })?;
                    let hazard = (intercept + slope * (lines as f64).ln_1p()).exp();
                    let probability = -(-hazard).exp_m1();
                    if probability > threshold {
                        return Err(refusal::refused(format!(
                            "selective receipt contradicted: owner-measured risk \
                             {probability:.6} exceeds the captured threshold, but the \
                             receipt records not_required"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// The owner's reading of the facts a before-PR certificate stands on
    /// [ORB-13895]: whether the reviewed base is in the history of the base
    /// the owner observed the candidate on, and the repository identity its
    /// coverage matches certificates against. `None` for a handoff carrying
    /// no before-PR evidence; the claim journal judges the rest.
    fn observe_handoff_review(
        &self,
        handoff: &TaskHandoff,
        candidate: &HandoffCandidate,
    ) -> Result<Option<HandoffReviewObservation>, OrbitError> {
        let Some(evidence) = handoff.review.evidence() else {
            return Ok(None);
        };
        let repo_root = &self.paths().repo_root;
        let repository = Source::new(repo_root)
            .repository()
            .map_err(orbit_automation::automation_error_to_orbit)?;
        Ok(Some(HandoffReviewObservation {
            reviewed_base_sha: evidence.reviewed_base_sha.clone(),
            reviewed_base_is_ancestor: orbit_engine::review_gate::contains_commit(
                repo_root,
                &evidence.reviewed_base_sha,
                &candidate.base.commit,
            )?,
            repository,
        }))
    }
}
