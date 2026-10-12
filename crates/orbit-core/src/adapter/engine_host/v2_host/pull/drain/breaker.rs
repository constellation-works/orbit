//! The pull drain's failure breaker [ORB-15639].
//!
//! A leaf that fails fast frees its slot within seconds, so without a breaker
//! a systemic executor fault — a missing credential, a broken toolchain, an
//! incompatible owner — would claim and block the owner's backlog one task per
//! poll. After [`CONSECUTIVE_FAILURE_BREAKER`] claims in a row settle as
//! failures the breaker opens and the drain requests no new work; settlement
//! of work already running is unaffected.
//!
//! The breaker does not latch for the drain's life: the cause is often fixed
//! while the drain runs. After a cooldown it half-opens and admits one probe
//! claim, once nothing the drain claimed is still unsettled, so the probe's
//! outcome is the next evidence. A probe that succeeds ends the streak and
//! closes it. One that fails lengthens the streak and opens it again, its
//! cooldown doubled for each failure past the threshold, up to
//! [`MAX_BACKOFF_DOUBLINGS`]. A probe released back to the owner (a provider,
//! a red base, a review hold) leaves the streak as it was, and the next pass
//! may probe again.
//!
//! Only claims whose failure could be this host's count: released claims and
//! Orbit's refusals of an implementer's handoff do not
//! ([`orbit_store::contracts::ClaimFailure::trips_breaker`]).
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::PullFailureStreak;
use orbit_types::workflow::PullBreaker;

/// Consecutive claims a drain settled as failures, with no handoff between
/// them, after which it stops requesting new work.
pub(crate) const CONSECUTIVE_FAILURE_BREAKER: usize = 3;

/// The cooldown before an open breaker half-opens, when the drain's input
/// sets none: long enough for an operator to fix a host, short enough that a
/// fixed host does not sit idle for the rest of its window.
pub(crate) const DEFAULT_BREAKER_COOLDOWN_SECONDS: u64 = 15 * 60;

/// How many times a failed probe doubles the cooldown, at most: eight times
/// the cooldown, two hours at the default.
const MAX_BACKOFF_DOUBLINGS: u32 = 3;

/// What the breaker allows a pass to request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BreakerGate {
    /// No breaker: request up to the free slots.
    Closed,
    /// Request nothing until it half-opens.
    Open,
    /// Request one probe claim, once nothing the drain claimed is unsettled.
    HalfOpen,
}

impl BreakerGate {
    /// The state's name in a pass's output.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::HalfOpen => "half_open",
        }
    }
}

/// The breaker `streak` leaves at `now`, given the one the previous pass
/// recorded. A streak below the threshold closes it. A streak the recorded
/// breaker already opened on keeps it, and its clock; any other opens it
/// afresh at `now`, its cooldown doubled for each failure past the threshold.
pub(crate) fn settle_breaker(
    previous: Option<&PullBreaker>,
    streak: &PullFailureStreak,
    now: DateTime<Utc>,
    cooldown: Duration,
) -> Option<PullBreaker> {
    let past_threshold = streak.failures().checked_sub(CONSECUTIVE_FAILURE_BREAKER)?;
    let failures = u32::try_from(streak.failures()).unwrap_or(u32::MAX);
    if let Some(previous) = previous.filter(|previous| previous.failures == failures) {
        return Some(previous.clone());
    }
    let doublings = u32::try_from(past_threshold)
        .unwrap_or(u32::MAX)
        .min(MAX_BACKOFF_DOUBLINGS);
    let wait = cooldown
        .checked_mul(1 << doublings)
        .unwrap_or(Duration::MAX);
    Some(PullBreaker {
        failures,
        failure_classes: streak.classes.clone(),
        opened_at: now,
        half_open_at: now
            .checked_add_signed(wait)
            .unwrap_or(DateTime::<Utc>::MAX_UTC),
    })
}

/// What `breaker` allows at `now`.
pub(crate) fn breaker_gate(breaker: Option<&PullBreaker>, now: DateTime<Utc>) -> BreakerGate {
    match breaker {
        None => BreakerGate::Closed,
        Some(breaker) if breaker.half_open(now) => BreakerGate::HalfOpen,
        Some(_) => BreakerGate::Open,
    }
}

/// Whether a pass may send its next request: `probed` says whether this pass
/// already took the half-open probe, and `unsettled` how many admissions the
/// drain carries that have not settled.
pub(crate) fn admits_request(gate: BreakerGate, probed: bool, unsettled: usize) -> bool {
    match gate {
        BreakerGate::Closed => true,
        BreakerGate::Open => false,
        BreakerGate::HalfOpen => !probed && unsettled == 0,
    }
}
