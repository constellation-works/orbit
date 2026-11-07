//! Visual delivery's side of the distributed-owned port [ORB-22624].

use orbit_common::OrbitError;
use orbit_types::task::{Task, VisualReviewContract};
use orbit_types::workflow::handoff::TaskHandoff;

use crate::OrbitRuntime;
use crate::application::distributed::DistributedVisualPort;

impl DistributedVisualPort for OrbitRuntime {
    fn claim_visual_review_contract(
        &self,
        task: &Task,
    ) -> Result<Option<VisualReviewContract>, OrbitError> {
        self.captured_visual_review(task)
    }

    fn recheck_observed_live_visual_resolution(
        &self,
        handoff: &TaskHandoff,
    ) -> Result<(), OrbitError> {
        self.check_handoff_live_visual_resolution(handoff)
    }
}
