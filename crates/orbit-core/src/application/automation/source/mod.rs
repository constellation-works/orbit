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
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// Overall budget for every git and provider command one source pass runs.
pub(super) const SOURCE_DEADLINE: Duration = Duration::from_secs(30);

/// A delivery pass could not fetch `origin/<branch>`. Observation is not
/// advanced and the local branch is not consulted in its place.
pub(crate) const SOURCE_FETCH_FAILED: &str = "source_fetch_failed";

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
