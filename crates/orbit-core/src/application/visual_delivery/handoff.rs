//! The owner's check of a claimed leaf's returned capture (§6.2): every
//! image's digest matches `capture.json`, every commit is the handed-off
//! head (or the head beneath its review commit), the manifest and mock
//! digests are the claim's contract, and the kit pin is this build's. A
//! mismatch refuses the handoff with `visual_capture_mismatch`; the owner
//! never relabels an image.

use orbit_common::OrbitError;
use orbit_types::task::{
    VISUAL_CAPTURE_MISMATCH, VisualCaptureExpectation, VisualCaptureRecord, is_visual_artifact,
    mock_sha256_hex, visual_capture_mismatches,
};
use orbit_types::workflow::handoff::{HandoffDelivery, TaskHandoff};

use crate::OrbitRuntime;
use crate::bootstrap::browser_kit::PLAYWRIGHT_VERSION;

impl OrbitRuntime {
    /// Refuse `handoff` when the capture it returns is not the one its
    /// claim's visual review contract asks for. A claim admitted without a
    /// contract (the phase did not apply) returns none and is not checked.
    pub(crate) fn check_handoff_visual_capture(
        &self,
        handoff: &TaskHandoff,
    ) -> Result<(), OrbitError> {
        let contract = self
            .claim_receipt(&handoff.claim_id)?
            .visual_review
            .filter(|contract| contract.task_id == handoff.task_id);
        let Some(contract) = contract else {
            return match &handoff.visual {
                Some(_) => Err(mismatch(
                    "the handoff returns a visual capture its claim captured no contract for",
                )),
                None => Ok(()),
            };
        };
        // A verified clean base delivers no change to capture.
        if matches!(
            handoff.candidate.delivery,
            HandoffDelivery::NoDiff { .. } | HandoffDelivery::AlreadyLanded { .. }
        ) {
            return Ok(());
        }
        let Some(reference) = &handoff.visual else {
            return Err(mismatch(
                "the claim captured a visual review contract, and the handoff returns no capture",
            ));
        };
        if !is_visual_artifact(&reference.path) {
            return Err(mismatch(format!(
                "the capture `{}` is outside the reserved `visual/` prefix",
                reference.path
            )));
        }
        let stored = self
            .get_task_artifact(&handoff.task_id, &reference.path)?
            .ok_or_else(|| {
                mismatch(format!("the capture `{}` was not returned", reference.path))
            })?;
        if mock_sha256_hex(&stored.content) != reference.sha256 {
            return Err(mismatch(format!(
                "the stored `{}` differs from the digest the handoff names",
                reference.path
            )));
        }
        let record = VisualCaptureRecord::parse(&stored.content)
            .map_err(|error| mismatch(format!("`{}` is unreadable: {error}", reference.path)))?;
        let head = handoff.candidate.candidate.commit.clone();
        let mut commits = vec![head.clone()];
        // A review commit made after capture sits on top of the captured head.
        if handoff
            .review
            .evidence()
            .and_then(|evidence| evidence.reviewer_commit.as_deref())
            == Some(head.as_str())
            && let Ok(parent) = crate::application::tag_instructions::commit_at(
                &self.paths().repo_root,
                &format!("{head}^"),
            )
        {
            commits.push(parent);
        }
        let task_id = handoff.task_id.clone();
        let stored_digest = |path: &str| {
            self.get_task_artifact(&task_id, path)
                .ok()
                .flatten()
                .map(|artifact| mock_sha256_hex(&artifact.content))
        };
        let mismatches = visual_capture_mismatches(
            &record,
            VisualCaptureExpectation {
                contract: &contract,
                commits: &commits,
                kit_pin: PLAYWRIGHT_VERSION,
            },
            &stored_digest,
        );
        if mismatches.is_empty() {
            return Ok(());
        }
        Err(mismatch(mismatches.join("; ")))
    }
}

fn mismatch(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!("{VISUAL_CAPTURE_MISMATCH}: {detail}"))
}
