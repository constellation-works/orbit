//! What the review gate and the handoff seam need from visual delivery
//! [ORB-22624].
//!
//! The reviewer resolves a live visual review's confirmed findings, and a
//! handoff's returned capture must be its contract's. `visual_delivery` owns
//! both and takes its own review admission from this module, so a direct call
//! from here would close a cycle. This port is the way in: `visual_delivery`
//! implements it on [`OrbitRuntime`](crate::OrbitRuntime), so composing the
//! runtime binds it.

use orbit_common::OrbitError;
use orbit_types::workflow::ReviewVisualFinding;
use orbit_types::workflow::handoff::TaskHandoff;

/// The visual reads review code calls, implemented by `visual_delivery`.
pub(in crate::application) trait ReviewVisualPort {
    /// The confirmed findings of `task_id`'s live visual review the code
    /// reviewer resolves; none in shadow, for any other task, or when the
    /// record cannot be read (review admission never fails on it).
    fn review_manifest_visual_findings(
        &self,
        task_id: &str,
    ) -> Result<Vec<ReviewVisualFinding>, OrbitError>;

    /// Refuse `handoff` when the capture it returns is not the one its claim's
    /// visual review contract asks for.
    fn refuse_mismatched_handoff_capture(&self, handoff: &TaskHandoff) -> Result<(), OrbitError>;
}
