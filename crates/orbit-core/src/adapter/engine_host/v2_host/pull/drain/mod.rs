//! Pull refill loop. The owner transport and the leaf launcher are injected;
//! the loop owns only the durable checkpoint discipline between them.
mod failure;
mod reconcile;
mod refill_pass;
mod settlement;

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionLookup, AdmissionReceipt, AdmissionRequest, JobRunStoreBackend, LocalPullAdmission,
    PullDestination,
};

use crate::application::job::pipeline::WorkerLaunchError;

#[cfg(test)]
pub(crate) use failure::release_settlement;
pub(crate) use failure::{leaf_failure_settlement, operator_cancel_release};

/// Trusted owner transport, supplied by runtime composition. Implementations
/// must check current claim/run/phase on bind and settlement; replaying a receipt
/// never supplies execution authority. There is no local fallback.
pub(crate) trait PullPeer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError>;
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    /// Read-only receipt reconciliation for one of this executor's request
    /// IDs. It never admits, and it does not reapply the version, ship or
    /// policy checks a replay of the request itself would.
    fn lookup(
        &self,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError>;
}

/// A launcher takes the existing bound run, never submits a replacement.
/// Success means that launch was acknowledged, not that execution completed.
pub(crate) trait PullLauncher {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), WorkerLaunchError>;
    /// Cancel a bound leaf that was never launched, so it can never start.
    /// Also used after a confirmed pre-spawn launch failure. A leaf that
    /// has started is refused, so a release cannot permit concurrent work.
    fn cancel_queued(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
}

/// How far a settle-only pass may carry an admission [ORB-13663].
///
/// A settle-only pass never requests, binds for execution, or launches work,
/// so any follower process may run one — a new drain, `orbit run cancel`,
/// `orbit run auto --stop`, or the leaf's own worker as it terminalizes. The
/// admission record is the outbox: whatever a pass cannot deliver stays
/// recorded for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleScope {
    /// Deliver recorded settlements, and record the failure a terminal leaf
    /// implies. Admissions still waiting on their drain are left to it.
    Deliver,
    /// No live drain will carry this admission forward — its own drain ended
    /// and none pulls from its owner: release what was never launched back to
    /// the owner, cancelling a queued leaf, so the task returns to the backlog
    /// and the owner is never left holding a claim no follower process is
    /// responsible for. Nothing ran, so nothing is failed. Live leaves are
    /// left running; they settle themselves when they terminalize.
    Abandon,
    /// The pass of a drain that is being cancelled gracefully, over its own
    /// owner's admissions: release everything unlaunched as
    /// [`Self::Abandon`] does, without asking whether a drain is live (this
    /// one is, and it is the one giving the work back), and withdraw a
    /// request the owner holds no receipt for, so the drain can finish. Live
    /// leaves are waited for.
    Cancel,
}

/// When a pass delivers a settlement the owner refused while still holding
/// its claim [ORB-13979]. Such a refusal is an answer that repeats until an
/// operator changes the owner, so automatic passes back off instead of asking
/// on every one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusedDelivery {
    /// Once its backoff has elapsed: a drain pass, the clock sweep, a leaf's
    /// own worker.
    WhenDue,
    /// Now, whatever the backoff: an operator's settle-only pass, so an
    /// operator who fixed the owner need not wait it out.
    Now,
}

/// Consecutive claims a drain settled as failures, with no handoff between
/// them, after which it stops requesting new work.
///
/// A leaf that fails fast frees its slot within seconds, so without a breaker
/// a systemic executor fault — a missing credential, a broken toolchain, an
/// incompatible owner — would claim and block the owner's backlog one task per
/// poll. Settlement of work already running is unaffected, and an operator
/// resets the breaker by starting a new drain.
pub(crate) const CONSECUTIVE_FAILURE_BREAKER: usize = 3;

/// What one [`PullDrain::refill_pass`] did: the claims it admitted, the
/// error that ended it, if one did, and the owner's last answer. The three are
/// independent — a pass can admit claims and then fail on a later one.
pub(crate) struct RefillPass {
    pub(crate) admitted: usize,
    pub(crate) error: Option<OrbitError>,
    /// The last receipt the owner returned to a request this pass sent or
    /// retried [ORB-14475]: its diagnostics are what the owner kept off this
    /// executor. `None` when the pass sent no request, or ended in an error
    /// before one was answered.
    pub(crate) answer: Option<Box<AdmissionReceipt>>,
}

/// How far one admission carried, and what that leaves the pass free to do.
enum Reconciled {
    /// Nothing the admission owes holds new requests back.
    Open,
    /// A settlement the owner has not accepted holds new requests back.
    Held,
    /// The owner answered a request idle: nothing ready for this executor.
    Idle(Box<AdmissionReceipt>),
}

/// How the owner answered a bind.
enum Bind {
    Bound(Box<LocalPullAdmission>),
    Refused(OrbitError),
}

pub(crate) struct PullDrain<'a> {
    pub(crate) jobs: &'a dyn JobRunStoreBackend,
    pub(crate) peer: &'a dyn PullPeer,
    pub(crate) launcher: &'a dyn PullLauncher,
    pub(crate) refused_delivery: RefusedDelivery,
}
