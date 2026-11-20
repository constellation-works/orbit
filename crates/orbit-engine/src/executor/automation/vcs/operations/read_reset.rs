//! Waiting out a GitHub rate limit on a workflow prerequisite read [ORB-25864].
//!
//! The automation account's REST quota is shared by every pipeline on the
//! host, so a rate-limited prerequisite read says nothing about the
//! candidate. The snapshot waits until the provider's not-before
//! ([`read_not_before`]) and reads again, at most
//! [`MAX_THROTTLED_READ_WAITS`] times, and only while that not-before falls
//! before the completion's absolute deadline, which is never extended.
//! That bound is per snapshot, not a global cap: the reads after the
//! snapshot (run, jobs, pinned workflow file, check suite) wait the same way
//! through [`read_through_throttles`], with their own count, so the deadline
//! alone bounds the whole operation. A wait only ever precedes a repeated
//! read; a reserved mutation is never part of what is read again.
//! `gh` names no reset in its rendered diagnostic, so the reset is read from
//! GitHub's `rate_limit` endpoint, which spends no quota. Anything this
//! policy refuses keeps the read's original failure.

use orbit_common::{OrbitError, ProviderReadThrottle};
use serde_json::Value;
use std::path::Path;

use super::super::completion_deadline::{Deadline, now_ms};
use crate::executor::automation::read_throttle::{read_not_before, rfc3339_ms};

/// Waits one snapshot may take for rate-limited reads.
const MAX_THROTTLED_READ_WAITS: u64 = 3;

/// Run `read` (a whole read-only pass over the prerequisite evidence) and,
/// when it fails on a rate-limited read whose reset fits before `deadline`,
/// wait for the reset and run it again, so every authenticated read it
/// makes is repeated. Any other failure, and a reset outside the deadline,
/// returns the read's original error unchanged.
pub(super) fn read_through_throttles<T>(
    deadline: Deadline,
    workspace: &Path,
    mut read: impl FnMut() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let mut waits = 0;
    loop {
        match read() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !wait_for_reset(deadline, workspace, &error, &mut waits) {
                    return Err(error);
                }
            }
        }
    }
}

/// When `error` is a rate-limited read and its reset fits before
/// `deadline`, wait for it, count the wait in `waits` and return `true`, so
/// the caller reads again. Otherwise return `false` without waiting.
pub(super) fn wait_for_reset(
    deadline: Deadline,
    workspace: &Path,
    error: &OrbitError,
    waits: &mut u64,
) -> bool {
    let Some(mut throttle) = error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
    else {
        return false;
    };
    if *waits >= MAX_THROTTLED_READ_WAITS {
        return false;
    }
    if throttle.retry_after_seconds.is_none() && throttle.reset_epoch_seconds.is_none() {
        throttle.reset_epoch_seconds = exhausted_reset(deadline, workspace, &throttle);
    }
    let Ok(now) = now_ms() else {
        return false;
    };
    let attempt = waits.saturating_add(1);
    let not_before = read_not_before(&throttle, now, attempt);
    if not_before.at_ms >= deadline.0 {
        tracing::warn!(
            target: "orbit_engine::executor::automation::vcs::operations",
            not_before = %rfc3339_ms(not_before.at_ms),
            deadline = %rfc3339_ms(deadline.0),
            "GitHub rate-limited a workflow prerequisite read past the completion deadline"
        );
        return false;
    }
    tracing::warn!(
        target: "orbit_engine::executor::automation::vcs::operations",
        attempt,
        not_before = %rfc3339_ms(not_before.at_ms),
        provider_named = not_before.provider_named,
        principal = throttle.principal.as_deref().unwrap_or("unavailable"),
        "GitHub rate-limited a workflow prerequisite read; waiting for the reset"
    );
    super::super::remote_probe::fixed_wait(std::time::Duration::from_millis(
        not_before.at_ms.saturating_sub(now),
    ));
    *waits = attempt;
    true
}

/// The latest reset of an exhausted rate-limit resource the throttle could
/// have hit (the one it named, `core` and `graphql`), from GitHub's
/// `rate_limit` endpoint, which does not count against the quota. `None`
/// when the read fails or no such resource is exhausted, which leaves the
/// unknown-reset backoff.
fn exhausted_reset(
    deadline: Deadline,
    workspace: &Path,
    throttle: &ProviderReadThrottle,
) -> Option<u64> {
    let response = deadline
        .execute(
            "gh",
            vec!["api".into(), "rate_limit".into()],
            workspace,
            "rate limit reset read",
        )
        .ok()?;
    let limits: Value = serde_json::from_str(&response.stdout).ok()?;
    ["core", "graphql"]
        .into_iter()
        .chain(throttle.resource.as_deref())
        .filter_map(|resource| limits["resources"].get(resource))
        .filter(|limit| limit["remaining"].as_u64() == Some(0))
        .filter_map(|limit| limit["reset"].as_u64())
        .max()
}
