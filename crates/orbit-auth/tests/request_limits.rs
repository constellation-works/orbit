//! Public-boundary regressions for key churn bypass and rejection amplification.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_auth::limiter::{KeyedLimiter, LimitKey};

#[test]
fn active_budgets_survive_key_churn_and_a_flood_coalesces_into_one_window() {
    let now = Instant::now();
    let period = Duration::from_secs(60);
    let mut limiter = KeyedLimiter::new(4, 64, now);
    let first = limiter.bounded_key(LimitKey::Address("192.0.2.1".parse().unwrap()));
    let at = Utc::now();
    assert!(
        limiter
            .check(&first, "address_requests", 1, period, now, true)
            .is_ok()
    );
    let mut snapshots = Vec::new();
    // Deterministic in-process state transitions at one injected instant,
    // never a network load test or parallel test hammer on the shared host.
    for n in 0..10_000u32 {
        let _ = limiter.bounded_key(LimitKey::Address(IpAddr::V4(Ipv4Addr::from(n))));
        assert!(
            limiter
                .check(&first, "address_requests", 1, period, now, true)
                .is_err(),
            "new keys must not evict the first active budget"
        );
        limiter.record_rejection(&first, "address_requests", period, now, at);
        snapshots.extend(limiter.drain_audits(now, false));
        assert!(limiter.tracked_keys() <= 4);
    }
    snapshots.extend(limiter.drain_audits(now, true));
    assert_eq!(snapshots.last().unwrap().count, 10_000);
    assert!(snapshots.iter().all(|row| row.first_at == at));
    assert!(
        limiter
            .check(&first, "address_requests", 1, period, now + period, true)
            .is_ok()
    );
    // A late first rejection must still roll over with the actual rate window.
    limiter.record_rejection(
        &first,
        "address_requests",
        period,
        now + period,
        at + chrono::Duration::seconds(60),
    );
    let next = limiter.drain_audits(now + period, true);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].count, 1);
}

#[test]
fn overflow_budgets_do_not_reset_and_open_streams_keep_their_allowances() {
    let now = Instant::now();
    let period = Duration::from_secs(60);
    let mut limiter = KeyedLimiter::new(3, 64, now);
    let token = limiter.bounded_key(LimitKey::Token("lt_first".into()));
    let stream = limiter.token_allowance(&token, 2, 1, true).unwrap();
    assert_eq!(
        limiter.token_allowance(&token, 2, 1, true).unwrap_err(),
        "token_streams"
    );
    let request = limiter.token_allowance(&token, 2, 1, false).unwrap();
    assert_eq!(
        limiter.token_allowance(&token, 2, 1, false).unwrap_err(),
        "token_in_flight"
    );
    limiter.drain_audits(now + period, true);
    assert_eq!(
        limiter.bounded_key(LimitKey::Token("lt_second".into())),
        LimitKey::TokenOverflow
    );
    let overflow = limiter.bounded_key(
        limiter.address_key(
            "[::ffff:192.0.2.1]"
                .trim_matches(['[', ']'])
                .parse()
                .unwrap(),
        ),
    );
    assert_eq!(overflow, LimitKey::AnonymousOverflow);
    assert!(
        limiter
            .check(&overflow, "address_requests", 1, period, now, true)
            .is_ok()
    );
    for n in 0..5 {
        let key = limiter.bounded_key(LimitKey::Address(Ipv4Addr::from(n).into()));
        assert!(
            limiter
                .check(&key, "address_requests", 1, period, now, true)
                .is_err()
        );
    }
    drop(request);
    drop(stream);
    assert!(limiter.token_allowance(&token, 2, 1, true).is_ok());
}
