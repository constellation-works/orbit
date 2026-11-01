//! [ORB-20771] Bounds and records the shared CI rerun planner's holds in an
//! owner-run completion.
//!
//! A planner hold spends no repair attempt, so without a deadline a red pull
//! request could poll silently until the completion budget ran out. Each hold
//! is recorded on the owning tasks once per head and reason. Past
//! [`CI_RERUN_HOLD_SECONDS`] on one head, or on the completion's last poll,
//! the hold expires: the failed checks go to the ordinary repair path, whose
//! own base and evidence gates admit a repair or stop with a reason.

use orbit_common::OrbitError;
use serde_json::json;

use crate::context::RuntimeHost;

use super::super::auto_fix::{AutoFix, record_base_path};

/// How long one head may hold on the CI rerun planner before its failed
/// checks go to the repair path.
pub(super) const CI_RERUN_HOLD_SECONDS: u64 = 15 * 60;

/// Task event recording a planner hold and its reason.
pub(super) const CI_RERUN_HOLD_EVENT: &str = "pr_auto_fix_ci_rerun_hold";

/// Task event recording that a planner hold expired into the repair path.
pub(super) const CI_RERUN_HOLD_EXPIRED_EVENT: &str = "pr_auto_fix_ci_rerun_hold_expired";

/// The head the planner holds on, the waited second the hold began, and
/// whether its expiry was recorded.
#[derive(Default)]
pub(super) struct CiRerunHold {
    current: Option<(String, u64, bool)>,
}

impl CiRerunHold {
    /// Forget the hold: the planner acted or did not hold on this poll.
    pub(super) fn clear(&mut self) {
        self.current = None;
    }

    /// Record the planner's hold and say whether it still holds this poll.
    /// `false` means it expired and the failed checks go to the repair path.
    /// A hold is recorded when it begins on a head and when it expires, so a
    /// reason that changes between polls cannot flood task history.
    pub(super) fn still_holds<H: RuntimeHost + ?Sized>(
        &mut self,
        host: &H,
        auto_fix: &AutoFix,
        head: Option<&str>,
        reason: &str,
        waited_seconds: u64,
        last_poll: bool,
    ) -> Result<bool, OrbitError> {
        let head = head.unwrap_or_default();
        let note = json!({
            "head": head,
            "reason": reason,
            "limit_seconds": CI_RERUN_HOLD_SECONDS,
        });
        let (since, expired) = match &mut self.current {
            Some((held, since, expired)) if held == head => (*since, expired),
            current => {
                record_base_path(host, auto_fix, CI_RERUN_HOLD_EVENT, &note)?;
                let (_, since, expired) = current.insert((head.to_owned(), waited_seconds, false));
                (*since, expired)
            }
        };
        if waited_seconds.saturating_sub(since) < CI_RERUN_HOLD_SECONDS && !last_poll {
            return Ok(true);
        }
        if !*expired {
            record_base_path(host, auto_fix, CI_RERUN_HOLD_EXPIRED_EVENT, &note)?;
            *expired = true;
        }
        Ok(false)
    }
}
