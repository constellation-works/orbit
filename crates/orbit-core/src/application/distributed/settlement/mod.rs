//! What a follower's settle-only pass did with each pull admission
//! [ORB-13663], and the crews a pull drain's window can run [ORB-13941].
//!
//! Settlement no longer belongs to the drain that admitted a claim. The
//! admission record is the outbox: the leaf's own worker records and delivers
//! its settlement as it terminalizes, and any other follower process —
//! `orbit run cancel`, `orbit run auto --stop`, a new drain — delivers whatever
//! is still recorded. This is the report those surfaces print, one entry per
//! admission that still held a slot when the pass started.

mod crew_window;
mod entries;
mod runtime;

pub(crate) use crew_window::OWNER_BUSY_EPISODE_LIMIT;
pub use crew_window::{OwnerBusyBackoff, PullCrewWindow};
pub(crate) use entries::settlement_refusal_backoff;
pub use entries::{
    ClaimedLeafStage, DrainClaimedLeaf, PendingPullSettlements, PullLeafClaim, PullSettlementEntry,
    PullSettlementOutcome, RefusedPullSettlement,
};
pub(crate) use runtime::{is_owner_refusal, is_owner_transport_failure};
