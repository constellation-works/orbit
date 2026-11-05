//! Owner verification of negotiated live claimed evidence. No worker verdict
//! grants landing authority: every pinned record is read again at the boundary.

use orbit_common::OrbitError;
use orbit_store::contracts::AdmissionReceipt;
use orbit_types::task::{
    CLAIMED_VISUAL_RESOLUTION_SCHEMA, ClaimedVisualResolution, VISUAL_RESOLUTION_ARTIFACT,
    VisualCaptureExpectation, VisualCaptureOutcome, VisualCaptureRecord, VisualChecksRecord,
    VisualPass, VisualReviewContract, VisualReviewMode, VisualReviewRecord, VisualVerdictKind,
    mock_sha256_hex, visual_capture_mismatches,
};
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffDelivery, TaskHandoff};
use orbit_types::workflow::{
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewManifest, ReviewReport,
};
use serde::de::DeserializeOwned;

use super::{attach_system_artifacts, json_artifact};
use crate::OrbitRuntime;
use orbit_exec::browser_kit::PLAYWRIGHT_VERSION;

fn held(detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "delivery_visual_review_unresolved: {detail}; candidate held for the operator"
    ))
}

impl OrbitRuntime {
    fn visual_evidence_ref(
        &self,
        task: &str,
        path: &str,
    ) -> Result<HandoffArtifactRef, OrbitError> {
        let stored = self
            .get_task_artifact(task, path)?
            .ok_or_else(|| held(format!("missing {path}")))?;
        Ok(HandoffArtifactRef {
            path: path.into(),
            sha256: mock_sha256_hex(&stored.content),
        })
    }

    fn pinned_visual_bytes(
        &self,
        task: &str,
        reference: &HandoffArtifactRef,
        path: &str,
    ) -> Result<Vec<u8>, OrbitError> {
        let stored = self
            .get_task_artifact(task, path)?
            .ok_or_else(|| held(format!("missing {path}")))?;
        if reference.path != path || mock_sha256_hex(&stored.content) != reference.sha256 {
            return Err(held(format!("mismatched evidence reference {path}")));
        }
        Ok(stored.content)
    }

    fn pinned_visual_record<T: DeserializeOwned>(
        &self,
        task: &str,
        reference: &HandoffArtifactRef,
        path: &str,
    ) -> Result<T, OrbitError> {
        let bytes = self.pinned_visual_bytes(task, reference, path)?;
        serde_json::from_slice(&bytes).map_err(|error| held(format!("invalid {path}: {error}")))
    }

    /// Seal a claimed leaf's resolution inputs after its last reviewer and
    /// recapture. The handoff pins this bundle instead of the initial capture.
    pub(super) fn record_claimed_visual_resolution(
        &self,
        task: &str,
        head: &str,
    ) -> Result<HandoffArtifactRef, OrbitError> {
        let binding = self
            .worker_invocation()
            .ok_or_else(|| held("claimed binding missing"))?;
        let contract = self
            .claimed_visual_review(task)?
            .ok_or_else(|| held("visual contract missing"))?;
        if contract.resolution_schema != Some(CLAIMED_VISUAL_RESOLUTION_SCHEMA) {
            return Err(held(
                "visual_live_claim_unsupported: resolution contract not negotiated",
            ));
        }
        let review_ref = self.visual_evidence_ref(task, &VisualPass::Delivery.review_artifact())?;
        let review: VisualReviewRecord =
            self.pinned_visual_record(task, &review_ref, &VisualPass::Delivery.review_artifact())?;
        let pass = if review.candidate_sha == head {
            VisualPass::Delivery
        } else {
            VisualPass::Recapture
        };
        let bundle = ClaimedVisualResolution {
            schema_version: CLAIMED_VISUAL_RESOLUTION_SCHEMA,
            task_id: task.into(),
            claim_id: binding.claim_id.clone(),
            run_id: binding.bound_run_id.clone(),
            candidate_sha: head.into(),
            contract_sha256: mock_sha256_hex(&serde_json::to_vec(&contract).map_err(held)?),
            initial_capture: self
                .visual_evidence_ref(task, &VisualPass::Delivery.capture_artifact())?,
            final_capture: self.visual_evidence_ref(task, &pass.capture_artifact())?,
            final_checks: self.visual_evidence_ref(task, &pass.checks_artifact())?,
            review: review_ref,
        };
        attach_system_artifacts(
            self,
            task,
            vec![json_artifact(VISUAL_RESOLUTION_ARTIFACT, &bundle)?],
        )?;
        self.visual_evidence_ref(task, VISUAL_RESOLUTION_ARTIFACT)
    }

    pub(super) fn check_claimed_visual_resolution(
        &self,
        handoff: &TaskHandoff,
        receipt: &AdmissionReceipt,
    ) -> Result<(), OrbitError> {
        let contract = receipt
            .visual_review
            .as_ref()
            .filter(|contract| {
                contract.task_id == handoff.task_id
                    && contract.mode == VisualReviewMode::Live
                    && contract.resolution_schema == Some(CLAIMED_VISUAL_RESOLUTION_SCHEMA)
                    && receipt.request.visual_resolution == CLAIMED_VISUAL_RESOLUTION_SCHEMA
                    && receipt.request.ship.mode == "pr"
            })
            .ok_or_else(|| {
                held("visual_live_claim_unsupported: no supported negotiated live contract")
            })?;
        // A verified clean base has no delivered visual change; the ordinary
        // no-diff owner verifier and exact-base checks still apply.
        if matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }) {
            return Ok(());
        }
        let reference = handoff
            .visual
            .as_ref()
            .ok_or_else(|| held("missing visual resolution bundle"))?;
        let bundle: ClaimedVisualResolution =
            self.pinned_visual_record(&handoff.task_id, reference, VISUAL_RESOLUTION_ARTIFACT)?;
        let head = &handoff.candidate.candidate.commit;
        let contract_digest = mock_sha256_hex(&serde_json::to_vec(contract).map_err(held)?);
        if bundle.schema_version != CLAIMED_VISUAL_RESOLUTION_SCHEMA
            || bundle.task_id != handoff.task_id
            || bundle.claim_id != handoff.claim_id
            || bundle.run_id != handoff.run_id
            || bundle.candidate_sha != *head
            || bundle.contract_sha256 != contract_digest
        {
            return Err(held(
                "unsupported, stale or foreign visual resolution bundle",
            ));
        }
        for mock in &contract.mocks {
            if self
                .visual_evidence_ref(&handoff.task_id, &mock.artifact)?
                .sha256
                != mock.sha256
            {
                return Err(held("mock_changed: frozen mock bytes differ"));
            }
        }
        crate::application::experiment::validate_experiment_capture(
            receipt.experiments.as_ref(),
            &handoff.task_id,
        )?;
        let reviewer_on = receipt
            .experiments
            .as_ref()
            .map(crate::application::experiment::captured_visual_reviewer)
            .transpose()?
            .flatten()
            .unwrap_or(contract.reviewer);
        let review: VisualReviewRecord = self.pinned_visual_record(
            &handoff.task_id,
            &bundle.review,
            &VisualPass::Delivery.review_artifact(),
        )?;
        if review.schema_version != 1
            || review.task_id != handoff.task_id
            || review.mode != VisualReviewMode::Live
            || review.recorded_only
            || review.purpose != "visual_review"
            || !review.not_established.is_empty()
            || !(review.outcome == "reviewed"
                || (!reviewer_on && review.outcome == "reviewer_disabled"))
        {
            return Err(held(
                "comparison review is not established under the captured contract",
            ));
        }
        let changed = review.candidate_sha != *head;
        if changed {
            let evidence = handoff
                .review
                .evidence()
                .ok_or_else(|| held("changed candidate has no review evidence"))?;
            let parent = crate::application::tag_instructions::commit_at(
                &self.paths().repo_root,
                &format!("{head}^"),
            )
            .map_err(held)?;
            if evidence.reviewer_commit.as_ref() != Some(head)
                || evidence.reviewed_head_sha != *head
                || parent != review.candidate_sha
            {
                return Err(held(
                    "comparison review does not pin the head beneath the final reviewer commit",
                ));
            }
        }
        let pass = if changed {
            VisualPass::Recapture
        } else {
            VisualPass::Delivery
        };
        let initial: VisualCaptureRecord = self.pinned_visual_record(
            &handoff.task_id,
            &bundle.initial_capture,
            &VisualPass::Delivery.capture_artifact(),
        )?;
        let final_capture: VisualCaptureRecord = self.pinned_visual_record(
            &handoff.task_id,
            &bundle.final_capture,
            &pass.capture_artifact(),
        )?;
        self.check_complete_claimed_capture(&initial, contract, &review.candidate_sha)?;
        self.check_complete_claimed_capture(&final_capture, contract, head)?;
        let checks: VisualChecksRecord = self.pinned_visual_record(
            &handoff.task_id,
            &bundle.final_checks,
            &pass.checks_artifact(),
        )?;
        if checks.schema_version != 1
            || checks.mode != VisualReviewMode::Live
            || checks.candidate_sha != *head
        {
            return Err(held("final checks are stale or unsupported"));
        }
        if changed
            && (review.recapture.as_ref().is_none_or(|recapture| {
                recapture.candidate_sha != *head
                    || recapture.outcome != "passed"
                    || !recapture.still_failing.is_empty()
            }) || checks
                .findings
                .iter()
                .any(|finding| contract.block_at.admits(&finding.severity)))
        {
            return Err(held(
                "final recapture is incomplete or still has blocking checks",
            ));
        }
        // Even a claim of dispute_accepted in a system record cannot replace
        // the operator's history on this owner.
        for finding in review.findings.iter().filter(|finding| {
            finding.verdict.verdict == VisualVerdictKind::Confirmed
                && contract.block_at.admits(finding.severity_of_record())
        }) {
            if super::resolve::owner_accepted_visual_dispute(self, &review, finding)? {
                continue;
            }
            let Some(evidence) = handoff.review.evidence() else {
                // With no configured reviewer, blocking findings are high
                // follow-ups under the existing local policy. A configured
                // but missing/skipped review never supplies resolution proof.
                if receipt.request.ship.review.is_none() {
                    continue;
                }
                return Err(held(format!(
                    "finding {} lacks reviewer resolution",
                    finding.finding_id
                )));
            };
            let read_review = |path: &str| -> Result<Vec<u8>, OrbitError> {
                let reference = evidence
                    .artifacts
                    .iter()
                    .find(|reference| reference.path == path)
                    .ok_or_else(|| held(format!("review evidence does not pin {path}")))?;
                self.pinned_visual_bytes(&handoff.task_id, reference, path)
            };
            let manifest: ReviewManifest =
                serde_json::from_slice(&read_review(REVIEW_MANIFEST_ARTIFACT)?).map_err(held)?;
            let report =
                ReviewReport::parse(&read_review(REVIEW_REPORT_ARTIFACT)?).map_err(held)?;
            if !changed
                || evidence.reviewer_commit.as_ref() != Some(head)
                || evidence.reviewed_head_sha != *head
                || manifest.attempt_id != evidence.attempt_id
                || report.attempt_id != evidence.attempt_id
                || manifest.candidate.commit != review.candidate_sha
                || !manifest.task_ids.contains(&handoff.task_id)
                || !manifest.visual_findings.iter().any(|issued| {
                    issued.finding_id == finding.finding_id
                        && issued.mock == finding.mock
                        && issued.question == finding.question
                        && issued.must_address
                })
                || report
                    .visual_resolutions
                    .iter()
                    .filter(|resolution| resolution.finding_id == finding.finding_id)
                    .count()
                    != 1
                || !report.visual_resolutions.iter().any(|resolution| {
                    resolution.finding_id == finding.finding_id && resolution.resolution == "fixed"
                })
            {
                return Err(held(format!(
                    "finding {} is unresolved; disputes require operator acceptance",
                    finding.finding_id
                )));
            }
        }
        Ok(())
    }

    fn check_complete_claimed_capture(
        &self,
        capture: &VisualCaptureRecord,
        contract: &VisualReviewContract,
        head: &str,
    ) -> Result<(), OrbitError> {
        let mismatches = visual_capture_mismatches(
            capture,
            VisualCaptureExpectation {
                contract,
                commits: &[head.to_string()],
                kit_pin: PLAYWRIGHT_VERSION,
            },
            &|path| {
                self.visual_evidence_ref(&contract.task_id, path)
                    .ok()
                    .map(|reference| reference.sha256)
            },
        );
        let (budgeted, _) = contract.budgeted_mocks();
        if !mismatches.is_empty() {
            return Err(held(mismatches.join("; ")));
        }
        if capture.schema_version != 1
            || capture.outcome != VisualCaptureOutcome::Captured
            || !capture.not_established.is_empty()
            || budgeted.is_empty()
            || capture.mocks.len() != budgeted.len()
            || capture.manifests
                != contract
                    .manifests
                    .iter()
                    .map(|manifest| manifest.reference.clone())
                    .collect::<Vec<_>>()
            || !budgeted.iter().all(|expected| {
                capture
                    .mocks
                    .iter()
                    .filter(|mock| {
                        mock.artifact == expected.artifact
                            && mock.sha256 == expected.sha256
                            && mock.view == expected.view
                            && mock.state == expected.state
                            && mock.theme == expected.theme
                            && mock.width == expected.width
                            && mock.error.is_none()
                            && mock
                                .capture
                                .as_ref()
                                .is_some_and(|image| image.commit == head)
                    })
                    .count()
                    == 1
            })
        {
            return Err(held(
                "capture is missing, partial or mismatched for the exact candidate",
            ));
        }
        Ok(())
    }
}
