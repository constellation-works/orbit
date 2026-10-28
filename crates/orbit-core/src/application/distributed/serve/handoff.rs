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
