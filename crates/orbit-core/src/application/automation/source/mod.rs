//! Bounded source facts from Git and provider-owned PR identities.

mod clock;
mod command;
mod inspection;
mod observe;
mod replay;
mod revision;
#[cfg(feature = "test-support")]
pub(crate) mod test_support;

pub(super) use clock::BudgetClock;
#[cfg(feature = "test-support")]
pub use clock::SOURCE_ELAPSED_ENV;
pub(super) use command::{LISTING_LIMIT, OUTPUT_LIMIT};
#[cfg(test)]
pub(super) use command::{arm_canonical_signature_deadline, clear_canonical_signature_deadline};
pub(super) use replay::is_batch_mismatch;
pub(crate) use revision::RemoteObservation;
#[cfg(test)]
pub(super) use revision::{arm_expire_source_after_next_head, clear_expired_source};

use super::source_cache::SourceCache;
use chrono::{DateTime, Utc};
use orbit_automation::{AutomationError, delivery::digest};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Overall budget for every git and provider command one source pass runs.
pub(super) const SOURCE_DEADLINE: Duration = Duration::from_secs(30);

struct ObservationLimits {
    commits: usize,
    lookups: usize,
}

pub(crate) struct Source<'a> {
    root: &'a Path,
    started: BudgetClock,
    cache: Option<&'a SourceCache>,
    now: DateTime<Utc>,
    fetch_origin: bool,
}

/// Keep persisted repository identities consistent across source readers.
/// Only repositories without a remote need to resolve the common directory.
fn repository_identity(
    remote: &str,
    common_dir: impl FnOnce() -> Result<String, AutomationError>,
) -> Result<String, AutomationError> {
    let remote = remote.trim();
    let normalized = remote.trim_end_matches(".git");
    if let Some(github) = normalized
        .strip_prefix("https://github.com/")
        .or_else(|| normalized.strip_prefix("git@github.com:"))
    {
        return Ok(github.to_owned());
    }
    // Non-GitHub identities hash the original remote, including `.git`.
    let identity = if remote.is_empty() {
        common_dir()?
    } else {
        remote.to_owned()
    };
    Ok(format!("git:{}", digest(identity.as_bytes())))
}

/// The source's monotonic budget clock; fixture clocks never affect UTC backoff.
fn budget_now() -> Instant {
    #[cfg(feature = "test-support")]
    if let Some(now) = test_support::now() {
        return now;
    }
    Instant::now()
}

impl Source<'_> {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}
