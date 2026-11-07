//! What claim admission and handoff observation need from visual delivery
//! [ORB-22624].
//!
//! The owner captures a claimed task's visual review contract in its admission
//! receipt and rechecks live evidence when it observes the handoff.
//! `visual_delivery` owns both and reads the claim receipt this module owns, so
//! a direct call from here would close a cycle. This port is the way in:
//! `visual_delivery` implements it on [`OrbitRuntime`](crate::OrbitRuntime), so
//! composing the runtime binds it.

use orbit_common::OrbitError;
use orbit_types::task::{Task, VisualReviewContract};
use orbit_types::workflow::handoff::TaskHandoff;

/// The visual reads distributed code calls, implemented by `visual_delivery`.
pub(in crate::application) trait DistributedVisualPort {
    /// The contract a claim admission returns in its receipt for `task`.
    fn claim_visual_review_contract(
        &self,
        task: &Task,
    ) -> Result<Option<VisualReviewContract>, OrbitError>;

    /// Recheck enforcing evidence before the owner observes `handoff`.
    fn recheck_observed_live_visual_resolution(
        &self,
        handoff: &TaskHandoff,
    ) -> Result<(), OrbitError>;
}
