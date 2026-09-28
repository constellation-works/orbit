//! What a follower's settle-only pass did with each pull admission
//! [ORB-13663].
//!
//! Settlement no longer belongs to the drain that admitted a claim. The
//! admission record is the outbox: the leaf's own worker records and delivers
//! its settlement as it terminalizes, and any other follower process —
//! `orbit run cancel`, `orbit run auto --stop`, a new drain — delivers whatever
//! is still recorded. This is the report those surfaces print, one entry per
//! admission that still held a slot when the pass started.

use serde::Serialize;

/// One admission carried by a settle-only pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullSettlementEntry {
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    pub request_id: String,
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// Where the admission stands after the pass:
    ///
    /// - `settled` — the owner accepted its handoff or failure;
    /// - `closed_obsolete` — the owner had already ended the claim, so the
    ///   settlement was closed locally (ORB-13639);
    /// - `leaf_running` — its leaf is live and settles itself when it ends;
    /// - `pending_delivery` — a settlement is recorded but did not reach the
    ///   owner; any later pass retries it;
    /// - `owner_unreachable` — skipped after an earlier delivery to the same
    ///   owner failed in this pass;
    /// - `awaiting_drain` — not launched yet, and a live drain for its owner
    ///   will carry it;
    /// - `unanswered_request` — no live drain will carry it and the owner
    ///   holds no receipt for the request, so nothing is held on the owner;
    /// - `launch_uncertain` — a launch was never acknowledged; deliberate
    ///   recovery is required;
    /// - `idle` / `refused` — an unanswered request the owner answered with
    ///   nothing ready, or refused, so no claim exists;
    /// - `pending` — stopped by the error in `detail`; any later pass retries;
    /// - `no_owner_route` — this runtime has no federated route to the owner.
    pub outcome: String,
    /// The error that stopped the admission, when one did.
    pub detail: Option<String>,
}

impl PullSettlementEntry {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        let subject = match (&self.task_id, &self.leaf_run_id) {
            (Some(task), Some(leaf)) => format!("{task} (leaf {leaf})"),
            (Some(task), None) => task.clone(),
            (None, Some(leaf)) => format!("leaf {leaf}"),
            (None, None) => format!("request {}", self.request_id),
        };
        match &self.detail {
            Some(detail) => format!("{subject}: {} — {detail}", self.outcome),
            None => format!("{subject}: {}", self.outcome),
        }
    }
}
