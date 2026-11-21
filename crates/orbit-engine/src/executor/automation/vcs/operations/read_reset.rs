//! Waiting out a GitHub rate limit on a workflow prerequisite read [ORB-25864].
//!
//! The automation account's REST quota is shared by every pipeline on the
//! host, so a rate-limited prerequisite read says nothing about the
//! candidate. The snapshot waits until the provider's not-before
//! ([`read_not_before`]) and reads again, at most
//! [`MAX_THROTTLED_READ_WAITS`] times, and only while that not-before falls
//! before the completion's absolute deadline, which is never extended.
//! That bound is per snapshot, not a global cap: the reads after the
//! snapshot (run, jobs, the prerequisite log's fallback, pinned workflow file,
//! check suite) wait the same way through [`read_through_throttles`], with
//! their own count, so the deadline alone bounds the whole operation. A
//! snapshot that spent its waits ends the whole pass with the provider's
//! diagnostic ([`SnapshotWaits`]): the pass is never read again over it, which
//! would hand the snapshot a fresh count. A wait
//! only ever precedes a repeated read; a reserved mutation is never part of
//! what is read again.
//! `gh` names no reset in its rendered diagnostic, so the reset is read from
//! GitHub's `rate_limit` endpoint, which spends no quota, through the host's
//! shared quota record when the guard is on. Anything this
//! policy refuses keeps the read's original failure.

use orbit_common::{OrbitError, ProviderReadThrottle};
use orbit_tools::github_quota::SharedReset;
use serde_json::Value;
use std::cell::Cell;
use std::path::Path;

use super::super::completion_deadline::{Deadline, now_ms};
use crate::executor::automation::read_throttle::{read_not_before, rfc3339_ms};

/// Waits one snapshot may take for rate-limited reads.
const MAX_THROTTLED_READ_WAITS: u64 = 3;

/// Whether the snapshot inside one pass gave up on a rate-limited read: it
/// spent its own waits, or its reset falls outside the deadline. The pass
/// that holds this ends with the snapshot's error rather than being read
/// again, so the snapshot's wait count is never restarted.
#[derive(Debug, Default)]
pub(super) struct SnapshotWaits {
    spent: Cell<bool>,
}

impl SnapshotWaits {
    /// Record that the snapshot is returning a rate-limited error it will not
    /// wait out.
    pub(super) fn mark_spent(&self) {
        self.spent.set(true);
    }

    fn spent(&self) -> bool {
        self.spent.get()
    }
}

/// Run `read` (a whole read-only pass over the prerequisite evidence) and,
/// when it fails on a rate-limited read whose reset fits before `deadline`,
/// wait for the reset and run it again, so every authenticated read it
/// makes is repeated. Any other failure, a reset outside the deadline, and
/// a snapshot that already spent its waits ([`SnapshotWaits`]) return the
/// read's original error unchanged.
pub(super) fn read_through_throttles<T>(
    deadline: Deadline,
    workspace: &Path,
    mut read: impl FnMut(&SnapshotWaits) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let mut waits = 0;
    loop {
        let snapshot = SnapshotWaits::default();
        match read(&snapshot) {
            Ok(value) => return Ok(value),
            Err(error) => {
                if snapshot.spent() || !wait_for_reset(deadline, workspace, &error, &mut waits) {
                    return Err(error);
                }
            }
        }
    }
}

/// Whether `error` is a rate-limited read the provider named a throttle for.
pub(super) fn is_rate_limited(error: &OrbitError) -> bool {
    error
        .private_vcs_failure()
        .is_some_and(|failure| failure.read_throttle.is_some())
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
/// have hit (the one it named, `core` and `graphql`). With the host guard on
/// it comes from the shared quota record, so it never reads GitHub's
/// `rate_limit` endpoint again inside the refresh interval an admission probe
/// already used [ORB-25938]; with the guard off it reads the endpoint, which
/// does not count against the quota. `None` when the read fails or no such
/// resource is exhausted, which leaves the unknown-reset backoff.
fn exhausted_reset(
    deadline: Deadline,
    workspace: &Path,
    throttle: &ProviderReadThrottle,
) -> Option<u64> {
    let resources = ["core", "graphql"]
        .into_iter()
        .chain(throttle.resource.as_deref())
        .collect::<Vec<_>>();
    match super::process::shared_exhausted_reset(workspace, deadline.remaining().ok()?, &resources)
        .ok()?
    {
        SharedReset::Guarded(reset) => return reset,
        SharedReset::Unguarded => {}
    }
    let response = deadline
        .execute(
            "gh",
            vec!["api".into(), "rate_limit".into()],
            workspace,
            "rate limit reset read",
        )
        .ok()?;
    let limits: Value = serde_json::from_str(&response.stdout).ok()?;
    resources
        .into_iter()
        .filter_map(|resource| limits["resources"].get(resource))
        .filter(|limit| limit["remaining"].as_u64() == Some(0))
        .filter_map(|limit| limit["reset"].as_u64())
        .max()
}
