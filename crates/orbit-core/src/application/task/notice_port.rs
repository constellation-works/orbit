//! What a task's transition into done needs from fleet notices [ORB-23017].
//!
//! Notice closure on that transition is best effort: a failure is logged and
//! the closure tick repairs the durable row later. `notice` owns that policy
//! and also reads tasks for poster authority and linked-task status, so a
//! direct call from here would close a cycle. This port is the way in:
//! `notice` implements it on [`OrbitRuntime`](crate::OrbitRuntime) in
//! `notice/close.rs`, delegating to the existing materializer, so composing
//! the runtime binds it. Authorization, dedupe, revision checks, owner
//! fencing and closure timing stay in `notice`.

/// The notice closure task code calls when a task becomes done, implemented
/// by `notice`.
pub(in crate::application) trait TaskNoticePort {
    /// Store the closure of every open notice linked to `task_id` now that
    /// the task is done. Returns nothing: a failure is logged, never raised
    /// into the task update. The closure tick still repairs a missed write.
    fn close_linked_notices_for_done_transition(&self, task_id: &str);
}
