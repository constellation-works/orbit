//! The reports a pull drain's run pages print: refused settlements, pending
//! deliveries and a leaf's owner claim. The settle-only pass's per-admission
//! entry and a drain's claimed leaves are contracts in
//! `application::contracts`.

use chrono::{DateTime, Utc};
use orbit_store::contracts::{LocalPullAdmission, LocalPullPhase};
use serde::Serialize;

use crate::application::contracts::SETTLEMENT_REFUSAL_REMEDY;
pub use crate::application::contracts::{PullLeafClaim, RefusedPullSettlement};

/// The first wait after the owner refuses a pending settlement while it still
/// holds the claim [ORB-13979]. Each further refusal doubles it, up to
/// [`SETTLEMENT_REFUSAL_MAX_BACKOFF`].
pub(crate) const SETTLEMENT_REFUSAL_FIRST_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(60);

/// The longest an automatic pass waits between deliveries of a settlement
/// the owner keeps refusing, so a fixed owner still settles it within this.
pub(crate) const SETTLEMENT_REFUSAL_MAX_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// How long automatic passes wait after the owner refused a settlement
/// `refusals` times in a row.
pub(crate) fn settlement_refusal_backoff(refusals: u32) -> std::time::Duration {
    let doublings = refusals.saturating_sub(1).min(16);
    SETTLEMENT_REFUSAL_FIRST_BACKOFF
        .saturating_mul(1 << doublings)
        .min(SETTLEMENT_REFUSAL_MAX_BACKOFF)
}

impl RefusedPullSettlement {
    /// The refusal `record` is held on, when it is a `settling` admission
    /// the owner refused.
    pub(crate) fn of(record: &LocalPullAdmission) -> Option<Self> {
        if record.phase != LocalPullPhase::Settling {
            return None;
        }
        let refusal = record.settlement_refusal.as_ref()?;
        Some(Self {
            task_id: record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.clone()),
            leaf_run_id: record.leaf_run_id.clone(),
            owner: record.destination.selector.clone(),
            drain_run_id: record.request.run_context.run_id.clone(),
            reason: refusal.reason.clone(),
            code: refusal.code.clone(),
            refusals: refusal.refusals,
            first_refused_at: refusal.first_refused_at,
            last_refused_at: refusal.last_refused_at,
            retry_after: refusal.retry_after,
            remedy: SETTLEMENT_REFUSAL_REMEDY.to_string(),
        })
    }

    /// Whether this refusal is a delivery-evidence refusal: it holds only
    /// this settlement, so a pass may still claim other work.
    #[must_use]
    pub fn is_delivery_refusal(&self) -> bool {
        crate::application::distributed::is_delivery_refusal_code(self.code.as_deref())
    }

    /// Whether this refusal holds new claims for its owner: every refusal
    /// except a delivery-evidence one.
    #[must_use]
    pub fn holds_admission(&self) -> bool {
        !self.is_delivery_refusal()
    }

    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) owner {} refused it {} time(s) since {}: {}; next attempt after {} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id.as_deref().unwrap_or("-"),
            self.owner,
            self.refusals,
            self.first_refused_at.to_rfc3339(),
            self.reason,
            self.retry_after.to_rfc3339(),
            self.remedy
        )
    }
}

/// Settlements this follower recorded but never delivered to the owner.
///
/// A live or cancelling drain retries delivery on every pass, and a leaf's
/// own worker retries its settlement with a bounded backoff when no drain
/// carries its owner. What outlasts both is retried by the OS clock sweep,
/// or waits for the next drain or for an operator to run
/// `orbit run auto --stop` on a host without the clock. This is the
/// read-only summary `orbit doctor` reports so the wait is visible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PendingPullSettlements {
    /// Admissions whose outcome is recorded but not delivered.
    pub count: usize,
    /// When the oldest such admission's leaf ended, and so when its outcome
    /// was recorded. `None` when none is pending or no ended leaf run can be
    /// read for any of them.
    pub oldest_recorded_at: Option<DateTime<Utc>>,
}

impl PendingPullSettlements {
    /// How long the oldest pending settlement has waited, at `now`.
    #[must_use]
    pub fn oldest_age(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        self.oldest_recorded_at
            .map(|recorded| (now - recorded).max(chrono::Duration::zero()))
    }
}

impl PullLeafClaim {
    /// One line for a run page.
    #[must_use]
    pub fn describe(&self) -> String {
        let failure = self
            .failure_class
            .map(|class| format!(" failure={}", class.as_str()))
            .unwrap_or_default();
        format!(
            "task {} claim {} owner {} drain {} settlement={}{failure} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.claim_id.as_deref().unwrap_or("-"),
            self.owner,
            self.drain_run_id,
            self.settlement_phase,
            self.guidance
        )
    }
}
