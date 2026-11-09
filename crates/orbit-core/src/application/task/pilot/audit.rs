//! Assessment artifacts and the inline audit compatibility reader.

use orbit_common::security::{
    redaction::{redact_all, redact_all_json},
    release::sha256_hex,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::TaskArtifactUpdateParams;
use orbit_types::task::{ArtifactWriter, CommentReceipt, TaskArtifact, TaskComment};
use serde_json::{Value, json};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Resolve an audit sealed by a pilot comment. Older receipts contain the
    /// audit itself; new ones bind an artifact's exact bytes, so replacing an
    /// ordinary task artifact cannot change a pilot's admission evidence.
    pub(crate) fn task_pilot_audit(
        &self,
        task_id: &str,
        receipt: CommentReceipt<'_>,
    ) -> Result<Value, OrbitError> {
        let decode = |bytes: &[u8]| {
            serde_json::from_slice::<Value>(bytes)
                .map_err(|error| OrbitError::Execution(format!("decode pilot assessment: {error}")))
        };
        let reference = decode(receipt.body.as_bytes())?;
        let Some(path) = reference.get("assessment_artifact") else {
            // Compatibility-only from 2026-11-09. Retain for at least 30 days
            // and until the release/remaining-record checks permit retirement.
            return Ok(reference);
        };
        let path = path.as_str().ok_or_else(|| {
            OrbitError::Execution("pilot assessment artifact path must be a string".into())
        })?;
        orbit_types::task::validate_relative_artifact_path(path)?;
        let artifact = self.get_task_artifact(task_id, path)?.ok_or_else(|| {
            OrbitError::not_found(NotFoundKind::Artifact, format!("{task_id}/{path}"))
        })?;
        artifact.validate_inline_size(task_id)?;
        if reference["assessment_sha256"].as_str() != Some(sha256_hex(&artifact.content).as_str()) {
            return Err(OrbitError::Execution(
                "pilot assessment artifact digest mismatch".into(),
            ));
        }
        decode(&artifact.content)
    }

    /// Resolve pilot audits for readers that also consume ordinary comments.
    /// Raw task/comment projections continue returning the compact messages.
    pub(crate) fn resolve_task_pilot_comments(
        &self,
        task_id: &str,
        comments: &[TaskComment],
    ) -> Result<Vec<TaskComment>, OrbitError> {
        let mut resolved = comments.to_vec();
        for comment in &mut resolved {
            if comment.by != super::PILOT_ACTOR {
                continue;
            }
            let Some(receipt) = comment.receipt() else {
                continue;
            };
            // Leave malformed inline receipts to each reader's existing
            // failure policy. A declared artifact, however, must resolve.
            if serde_json::from_str::<Value>(receipt.body)
                .ok()
                .is_some_and(|body| body.get("assessment_artifact").is_some())
            {
                let operation_id = receipt.operation_id.to_string();
                comment.message = self.task_pilot_audit(task_id, receipt)?.to_string();
                comment.operation_id = Some(operation_id);
            }
        }
        Ok(resolved)
    }

    pub(crate) fn get_task_pilot_comments(
        &self,
        task_id: &str,
    ) -> Result<Vec<TaskComment>, OrbitError> {
        self.resolve_task_pilot_comments(task_id, &self.get_task_comments(task_id)?)
    }

    /// Publish evidence before its receipt under the caller's task write lock.
    /// A failed mutation can leave an unreferenced artifact, never a receipt
    /// pointing at evidence that has not been persisted. Replays use the same
    /// operation path and skip this write once their receipt exists.
    pub(super) fn persist_pilot_audit(
        &self,
        task_id: &str,
        operation_id: &str,
        summary: &str,
        audit: Value,
    ) -> Result<String, OrbitError> {
        let path = format!("pilot/assessment-{operation_id}.json");
        let content = serde_json::to_vec(&redact_all_json(audit)).map_err(|error| {
            OrbitError::Execution(format!("serialize pilot assessment: {error}"))
        })?;
        let digest = sha256_hex(&content);
        let artifact = TaskArtifact {
            path: path.clone(),
            media_type: "application/json".into(),
            content,
            created_by: None,
        };
        artifact.validate_inline_size(task_id)?;
        self.stores().task_artifacts().upsert_task_artifacts(
            task_id,
            TaskArtifactUpdateParams {
                origin: None,
                actor: super::PILOT_ACTOR.into(),
                owner_run_id: None,
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![artifact],
            },
        )?;
        Ok(json!({
            "summary": redact_all(summary),
            "assessment_artifact": path,
            "assessment_sha256": digest,
            "operation_id": operation_id,
        })
        .to_string())
    }
}
