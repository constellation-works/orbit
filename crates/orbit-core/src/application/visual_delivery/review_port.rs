//! Visual delivery's side of the review-owned port [ORB-22624].

use orbit_common::OrbitError;
use orbit_types::workflow::ReviewVisualFinding;
use orbit_types::workflow::handoff::TaskHandoff;

use crate::OrbitRuntime;
use crate::application::review::ReviewVisualPort;

impl ReviewVisualPort for OrbitRuntime {
    fn review_manifest_visual_findings(
        &self,
        task_id: &str,
    ) -> Result<Vec<ReviewVisualFinding>, OrbitError> {
        self.visual_findings_for_review(task_id)
    }

    fn refuse_mismatched_handoff_capture(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        self.check_handoff_visual_capture(handoff)
    }
}
