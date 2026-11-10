//! Listing, showing and answering human decision requests.
//!
//! Requests are raised on held work by block triage and pilot admission,
//! and on proposed and review tasks by the decision digest in
//! `decision_channel` (see `orbit_types::task::DecisionRequest`). They
//! live in task history, so listing scans task histories — the shape the
//! block readout already has.
//!
//! [`OrbitRuntime::answer_decision_request_as_human`] is the core apply
//! step every answering surface shares: the operator CLI, the dashboard,
//! and later the delegated channel that submits a human's answer to core.
//! It refuses a stale answer (superseded, expired, or an answer outside
//! the request's allowed set) with the design's named refusals and writes
//! nothing. The request is judged on a fresh read under the task's write
//! lock, which the gate's effect and the `decision_answered` record share,
//! so a task edit cannot land between the check and the effect. An allowed
//! pilot answer that cannot resolve the current duplicate or already-landed
//! hold is refused the same way, before any comment. Every other allowed answer lands through the human funnel
//! (`update_task_as_human`, or the guarded approval / `reject_task` for
//! approvals) and only then records `decision_answered`, which is what
//! makes the request read `answered`. A proposed approval is screened
//! first (`ensure_proposal_admissible`): work a pilot hold or a missing
//! prepared footprint would keep out of the backlog is refused the same
//! way, before any write.

mod answering;
mod handoff_landing;
mod listing;
mod precedent;
pub(super) mod raise;
mod rendering;
pub(crate) mod rules;

#[cfg(feature = "test-support")]
pub(crate) use answering::answer_checkpoint;
pub(crate) use listing::decision_request_json;
pub use listing::{DecisionListProfile, DecisionRequestListing};
