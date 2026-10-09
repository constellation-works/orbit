//! Bounded source facts from Git and provider-owned PR identities.

mod command;
mod observe;
mod replay;
mod revision;

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
const SOURCE_DEADLINE: Duration = Duration::from_secs(30);

/// A delivery pass could not fetch `origin/<branch>`. Observation is not
/// advanced and the local branch is not consulted in its place.
pub(crate) const SOURCE_FETCH_FAILED: &str = "source_fetch_failed";

struct ObservationLimits {
    commits: usize,
    lookups: usize,
}

pub(crate) struct Source<'a> {
    root: &'a Path,
    started: Instant,
    cache: Option<&'a SourceCache>,
    now: DateTime<Utc>,
    fetch_origin: bool,
}
