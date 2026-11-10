//! Review's side of the task-owned review port [ORB-17936].

use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, Task};
use orbit_types::workflow::ReviewAdmission;
use orbit_types::workflow::handoff::AcceptedHandoff;
use serde_json::Value;

use super::{evidence, reconciliation, task_review_projection};
use crate::OrbitRuntime;
use crate::application::task::{HandoffPullRequest, TaskReviewPort};

impl TaskReviewPort for OrbitRuntime {
    fn forecast_review_admission(&self) -> Result<ReviewAdmission, OrbitError> {
        super::admission::snapshot(self)
    }

    fn handoff_landing_review_ready(
        &self,
        task: &Task,
        head: Option<&str>,
    ) -> Result<bool, OrbitError> {
        use crate::application::job::ReviewAdmissionHook;
        use orbit_types::task::ArtifactWriter;
        use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewTiming};
        let captured = task
            .job_run_id
            .as_deref()
            .map(|id| self.captured_review_admission(id))
            .transpose()?
            .flatten();
        let gated = |timing| matches!(timing, ReviewTiming::BeforePr | ReviewTiming::BeforeLanding);
        let policy = self.operation_policy();
        if !policy.review_before_pr.value
            && !policy.review_before_landing.value
            && !captured.is_some_and(|admission| gated(admission.timing))
        {
            return Ok(true);
        }
        let Some(head) = head else {
            return Ok(false);
        };
        let manifest = self.get_task_artifact_manifest(&task.id)?;
        if !manifest.iter().any(|file| {
            file.path == REVIEW_GATE_ARTIFACT && file.writer == Some(ArtifactWriter::System)
        }) {
            return Ok(false);
        }
        let Some(artifact) = self.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)? else {
            return Ok(false);
        };
        let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
            return Ok(false);
        };
        if !certificate.verdict.passed()
            || !certificate.validation_complete
            || certificate.final_candidate.commit != head
            || !certificate.task_ids.contains(&task.id)
        {
            return Ok(false);
        }
        let meanings = certificate
            .task_ids
            .iter()
            .map(|id| {
                let task = self.get_task(id)?;
                let digest = orbit_automation::review::task_meaning_digest(&task)
                    .map_err(super::automation_error)?;
                Ok((id.clone(), digest))
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        if orbit_automation::review::combined_task_meaning_digest(&meanings)
            .map_err(super::automation_error)?
            != certificate.task_meaning_digest
        {
            return Ok(false);
        }
        // The immutable owner ledger authenticates the artifact's certificate.
        Ok(self
            .review_store()?
            .review_certificate(&self.workspace_id()?, &certificate.attempt_id)?
            .as_ref()
            == Some(&certificate))
    }

    fn task_review_evidence(
        &self,
        task: &Task,
        artifacts: &[ArtifactManifestFileV2],
    ) -> Result<Option<Value>, OrbitError> {
        task_review_projection(self, task, artifacts)
    }

    fn resume_review_evidence(&self, task_id: &str) -> Result<(), OrbitError> {
        evidence::resume_evidence_hold(self, task_id)
    }

    fn merged_head_reconciliation_refusal(
        &self,
        task: &Task,
        accepted: &AcceptedHandoff,
        pull_request: &HandoffPullRequest,
    ) -> Result<Option<String>, OrbitError> {
        reconciliation::merged_head_completion_refusal(self, task, accepted, pull_request)
    }
}
