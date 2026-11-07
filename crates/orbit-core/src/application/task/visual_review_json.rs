//! The stored `visual/*.json` records as task read surfaces serve them. A pure
//! artifact read: `visual_delivery` writes these records and reads them back
//! through [`read_visual_record`], and `task` owns the read so that task show
//! does not climb into the delivery phase [ORB-22624].

use orbit_common::OrbitError;
use serde_json::Value;

use crate::OrbitRuntime;

/// A stored `visual/*.json` record, parsed; `None` when it is absent.
pub(in crate::application) fn read_visual_record<T: serde::de::DeserializeOwned>(
    runtime: &OrbitRuntime,
    task_id: &str,
    path: &str,
) -> Result<Option<(T, Vec<u8>)>, OrbitError> {
    let Some(artifact) = runtime.get_task_artifact(task_id, path)? else {
        return Ok(None);
    };
    let record = serde_json::from_slice(&artifact.content)
        .map_err(|error| OrbitError::Execution(format!("decode {path} of {task_id}: {error}")))?;
    Ok(Some((record, artifact.content)))
}

impl OrbitRuntime {
    /// A `verify-mock-ui` task's last capture of record for task show:
    /// its summary line (never a pass when nothing was captured), outcome,
    /// `not_established` reasons, check findings and settlement. `None` for
    /// any other task, or one never captured.
    pub(super) fn task_visual_review_json(
        &self,
        task: &orbit_types::task::Task,
    ) -> Result<Option<Value>, OrbitError> {
        use orbit_types::task::{
            VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VISUAL_REVIEW_ARTIFACT,
            VISUAL_SETTLE_ARTIFACT, VisualCaptureRecord, VisualChecksRecord, VisualReviewRecord,
            VisualSettleRecord, has_verify_mock_ui_tag, visual_review_summary_with_review,
        };
        if !has_verify_mock_ui_tag(&task.tags) {
            return Ok(None);
        }
        let Some(capture) = self.get_task_artifact(&task.id, VISUAL_CAPTURE_ARTIFACT)? else {
            return Ok(None);
        };
        let Ok(record) = VisualCaptureRecord::parse(&capture.content) else {
            return Ok(Some(serde_json::json!({
                "summary": "capture record unreadable",
            })));
        };
        let checks =
            read_visual_record::<VisualChecksRecord>(self, &task.id, VISUAL_CHECKS_ARTIFACT)
                .ok()
                .flatten()
                .map(|(checks, _)| checks)
                .filter(|checks| checks.candidate_sha == record.candidate_sha);
        let settle =
            read_visual_record::<VisualSettleRecord>(self, &task.id, VISUAL_SETTLE_ARTIFACT)
                .ok()
                .flatten()
                .map(|(settle, _)| settle)
                .filter(|settle| settle.candidate_sha == record.candidate_sha);
        let review =
            read_visual_record::<VisualReviewRecord>(self, &task.id, VISUAL_REVIEW_ARTIFACT)
                .ok()
                .flatten()
                .map(|(review, _)| review)
                .filter(|review| review.candidate_sha == record.candidate_sha);
        Ok(Some(serde_json::json!({
            "summary": visual_review_summary_with_review(
                &record,
                checks.as_ref(),
                settle.as_ref(),
                review.as_ref(),
            ),
            "review": review.as_ref().map(|review| serde_json::json!({
                "outcome": review.outcome,
                "purpose": review.purpose,
                "reviewer_crew": review.reviewer_crew,
                "verifier_crews": review.verifier_crews,
                "findings": review.findings,
                "invalid_evidence_dropped": review.invalid_evidence_dropped,
                "not_shown": review.budget.not_shown,
                "recorded_only": review.recorded_only,
            })),
            "mode": record.mode.as_str(),
            "outcome": record.outcome,
            "candidate_sha": record.candidate_sha,
            "not_established": record.not_established,
            "findings": checks.as_ref().map(|checks| &checks.findings),
            "settle": settle.as_ref().map(|settle| serde_json::json!({
                "outcome": settle.outcome,
                "refusals": settle.refusals,
                "held": settle.held,
            })),
        })))
    }
}
