//! What task reads and writes need from review evidence [ORB-17936].
//!
//! Task records and the desktop snapshot render a task's review projection,
//! an evidence write resumes a review hold, and a desktop review completes a
//! changed merged head only on an accepted review reconciliation. `review`
//! owns all of that, and it also drives task writes, so a direct call from
//! here would close a cycle. This port is the way in: `review` implements it
//! on [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds
//! it.

use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, Task};
use orbit_types::workflow::handoff::AcceptedHandoff;
use serde_json::Value;

use super::HandoffPullRequest;

/// The review-evidence operations task code calls, implemented by `review`.
pub(in crate::application) trait TaskReviewPort {
    /// The review gate evidence for `task`, read from the certificate its
    /// `artifacts` list; `None` when no gate ever settled it.
    fn task_review_evidence(
        &self,
        task: &Task,
        artifacts: &[ArtifactManifestFileV2],
    ) -> Result<Option<Value>, OrbitError>;

    /// Resume the review evidence hold of in-progress task `task_id` once
    /// the evidence it waited for is attached.
    fn resume_review_evidence(&self, task_id: &str) -> Result<(), OrbitError>;

    /// Why the merged head of `pull_request`, which differs from the
    /// handed-off candidate, cannot complete `task`; `None` when an accepted
    /// review reconciliation covers exactly that head.
    fn merged_head_reconciliation_refusal(
        &self,
        task: &Task,
        accepted: &AcceptedHandoff,
        pull_request: &HandoffPullRequest,
    ) -> Result<Option<String>, OrbitError>;
}
