//! The failure breaker's state machine against an injected clock [ORB-15639]:
//! backoff arithmetic and half-open admission are pure, and a boundary test
//! cannot step a real drain's clock through a cooldown.
use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_store::contracts::PullFailureStreak;
use orbit_types::workflow::PullBreaker;

use super::super::{BreakerGate, admits_request, breaker_gate, settle_breaker};

fn at(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 11, 16, 0, 0).unwrap() + Duration::minutes(minutes)
}

fn streak(failures: usize) -> PullFailureStreak {
    PullFailureStreak {
        classes: vec!["candidate".to_string(); failures],
    }
}

/// One pass: the breaker it leaves and what that allows, from the breaker
/// the previous pass recorded.
fn pass(
    previous: Option<&PullBreaker>,
    failures: usize,
    minute: i64,
) -> (Option<PullBreaker>, BreakerGate) {
    let now = at(minute);
    let breaker = settle_breaker(previous, &streak(failures), now, Duration::minutes(15));
    let gate = breaker_gate(breaker.as_ref(), now);
    (breaker, gate)
}

#[test]
fn an_open_breaker_half_opens_after_its_cooldown_and_a_failed_probe_doubles_it() {
    // Below the threshold there is no breaker.
    assert_eq!(pass(None, 2, 0), (None, BreakerGate::Closed));

    // The third failure opens it for the cooldown.
    let (opened, gate) = pass(None, 3, 0);
    let opened = opened.expect("three failures open the breaker");
    assert_eq!(gate, BreakerGate::Open);
    assert_eq!((opened.failures, opened.opened_at), (3, at(0)));
    assert_eq!(opened.half_open_at, at(15));
    assert_eq!(opened.failure_classes, vec!["candidate"; 3]);

    // The same streak keeps its clock: open until the cooldown ends, then
    // half-open.
    assert_eq!(
        pass(Some(&opened), 3, 14),
        (Some(opened.clone()), BreakerGate::Open)
    );
    assert_eq!(
        pass(Some(&opened), 3, 15),
        (Some(opened.clone()), BreakerGate::HalfOpen)
    );

    // A failed probe reopens it at once, its cooldown doubled per failure past
    // the threshold.
    let (reopened, gate) = pass(Some(&opened), 4, 20);
    let reopened = reopened.expect("a failed probe reopens the breaker");
    assert_eq!(gate, BreakerGate::Open);
    assert_eq!(
        (reopened.opened_at, reopened.half_open_at),
        (at(20), at(50))
    );
    let (again, _) = pass(Some(&reopened), 5, 50);
    assert_eq!(again.expect("open").half_open_at, at(110));

    // The backoff stops doubling at eight times the cooldown.
    let (capped, _) = pass(None, 9, 0);
    assert_eq!(capped.expect("open").half_open_at, at(120));

    // A probe that succeeds ends the streak and closes the breaker.
    assert_eq!(pass(Some(&reopened), 0, 60), (None, BreakerGate::Closed));
}

#[test]
fn a_half_open_breaker_admits_one_probe_and_only_once_nothing_is_unsettled() {
    // Closed admits every request; open admits none.
    assert!(admits_request(BreakerGate::Closed, false, 4));
    assert!(admits_request(BreakerGate::Closed, true, 4));
    assert!(!admits_request(BreakerGate::Open, false, 0));
    // Half-open: one probe per pass, and none while a claim the drain carries
    // (the last probe, or a leaf admitted before the breaker opened) is
    // unsettled, so the probe's outcome is the next evidence.
    assert!(admits_request(BreakerGate::HalfOpen, false, 0));
    assert!(!admits_request(BreakerGate::HalfOpen, true, 0));
    assert!(!admits_request(BreakerGate::HalfOpen, false, 1));
}
