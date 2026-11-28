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
        manifest: Option<&[ArtifactManifestFileV2]>,
    ) -> Result<bool, OrbitError> {
        self.handoff_landing_review_ready_with(task, head, manifest, false)
    }

    fn observe_handoff_landing_review_ready(
        &self,
        task: &Task,
        head: Option<&str>,
        manifest: Option<&[ArtifactManifestFileV2]>,
    ) -> Result<bool, OrbitError> {
        self.handoff_landing_review_ready_with(task, head, manifest, true)
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
