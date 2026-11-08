//! Waiting out a provider rate limit on the landing's failed-check reads.
//!
//! The account's REST quota is shared by every landing, watcher and sweep on
//! the host, so a throttled job or log read says nothing about the failure:
//! classifying around it would read as an unclassified stop, and polling
//! again before the reset only spends more of the quota. The landing instead
//! waits until the provider's not-before
//! ([`read_not_before`](crate::executor::automation::read_throttle::read_not_before))
//! and records that wait, at most [`MAX_THROTTLED_READ_WAITS`] times in one
//! landing pass. A not-before at or past the landing deadline, or one more
//! throttled read after those waits, holds the run as a forge-observation
//! hold that resumes once the forge answers. Nothing is rerun, updated or
//! repaired either way.

use orbit_common::{OrbitError, ProviderReadThrottle};
use serde_json::{Value, json};

use crate::executor::automation::read_throttle::{read_not_before, rfc3339_ms};

/// Waits one landing pass may take for throttled check reads.
const MAX_THROTTLED_READ_WAITS: u64 = 3;

/// A failed-check read the provider rate-limited.
pub(in crate::executor::automation::vcs) struct ThrottledRead {
    /// What could not be read, with the provider's own diagnostic.
    pub(in crate::executor::automation::vcs) reason: String,
    pub(in crate::executor::automation::vcs) throttle: ProviderReadThrottle,
}

impl ThrottledRead {
    /// The throttled read behind `error`, or `None` for any other failure.
    pub(super) fn of(error: &OrbitError, reason: impl FnOnce() -> String) -> Option<Self> {
        let throttle = error.private_vcs_failure()?.read_throttle.clone()?;
        Some(Self {
            reason: reason(),
            throttle,
        })
    }
}

/// What the landing does about a throttled read.
pub(in crate::executor::automation::vcs) enum ReadWait {
    /// Sleep `delay_ms`, then poll again; `note` is the recorded wait.
    Wait { delay_ms: u64, note: Value },
    /// Hold for the forge with this evidence, which names the not-before.
    Hold(String),
}

/// The wait `read` earns at `now_ms`, given the `waits` this pass already
/// took and its `deadline_ms`, which is never extended.
pub(in crate::executor::automation::vcs) fn landing_read_wait(
    read: &ThrottledRead,
    waits: &mut u64,
    head: Option<&str>,
    now_ms: u64,
    deadline_ms: u64,
) -> ReadWait {
    let attempt = waits.saturating_add(1);
    let not_before = read_not_before(&read.throttle, now_ms, attempt);
    let at = rfc3339_ms(not_before.at_ms);
    let reset = if not_before.provider_named {
        format!("the provider's rate limit resets at {at}")
    } else {
        format!("the provider named no reset, so the read backs off until {at}")
    };
    let deferred = |why: &str| {
        format!(
            "{}; the provider rate-limited the read, so the failure is not classified until \
             the reset: {reset}; {why}; nothing is rerun, updated or repaired and the candidate \
             stays in review",
            read.reason
        )
    };
    if attempt > MAX_THROTTLED_READ_WAITS {
        return ReadWait::Hold(deferred(&format!(
            "the read stayed throttled after {MAX_THROTTLED_READ_WAITS} bounded waits, so the \
             landing is held for the forge"
        )));
    }
    if not_before.at_ms >= deadline_ms {
        return ReadWait::Hold(deferred(&format!(
            "that is not before the landing deadline ({}), which is never extended, so the \
             landing is held for the forge",
            rfc3339_ms(deadline_ms)
        )));
    }
    *waits = attempt;
    let throttle = &read.throttle;
    ReadWait::Wait {
        delay_ms: not_before.at_ms.saturating_sub(now_ms),
        note: json!({
            "state": "read_deferred",
            "reason": deferred("the landing waits for the reset and reads again"),
            "head": head,
            "attempt": attempt,
            "not_before_ms": not_before.at_ms,
            "deadline_ms": deadline_ms,
            "retry_after_seconds": throttle.retry_after_seconds,
            "reset_epoch_seconds": throttle.reset_epoch_seconds,
            "resource": throttle.resource,
            "principal": throttle.principal,
        }),
    }
}
