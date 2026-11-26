//! The Fleet counters at their rolling-window cutoffs, with fixed timestamps.

use std::net::IpAddr;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::state::remote_access::MAX_REFUSAL_KEYS;
use crate::state::{RefusalReason, RemoteAccess};

fn at(h: u32, m: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 25, h, m, s).unwrap()
}

fn ip(last: u8) -> Option<IpAddr> {
    Some(IpAddr::from([100, 64, 0, last]))
}

fn requests(access: &RemoteAccess, now: DateTime<Utc>) -> u64 {
    access
        .snapshot_at(now)
        .devices
        .iter()
        .map(|device| device.requests_15m)
        .sum()
}

fn refused(access: &RemoteAccess, now: DateTime<Utc>) -> u64 {
    access
        .snapshot_at(now)
        .refusals
        .iter()
        .map(|item| item.count_24h)
        .sum()
}

#[test]
fn a_request_late_in_a_minute_counts_for_the_full_fifteen_minutes() {
    let access = RemoteAccess::default();
    let sent = at(10, 0, 59);
    access.record_admitted_at(ip(1), "a@example.com", sent);

    // Wall-clock minute buckets dropped it at 10:15:00, 14 min 1 s after.
    for age in [0, 14 * 60 + 1, 15 * 60] {
        assert_eq!(
            requests(&access, sent + Duration::seconds(age)),
            1,
            "{age}s"
        );
    }
    assert_eq!(requests(&access, sent + Duration::seconds(15 * 60 + 1)), 0);
}

#[test]
fn requests_leave_the_window_one_second_at_a_time() {
    let access = RemoteAccess::default();
    for second in 0..3 {
        access.record_admitted_at(ip(1), "a@example.com", at(10, 0, second));
    }
    let counts: Vec<u64> = (0..4)
        .map(|second| requests(&access, at(10, 15, second)))
        .collect();
    assert_eq!(counts, [3, 2, 1, 0]);
}

#[test]
fn a_refusal_late_in_an_hour_counts_for_the_full_day() {
    let access = RemoteAccess::default();
    let sent = at(10, 59, 0);
    access.record_refused_at(RefusalReason::NoIdentity, None, sent);

    // Wall-clock hour buckets dropped it at 10:00 the next day, 23 h 1 min after.
    for age in [
        Duration::zero(),
        Duration::minutes(23 * 60 + 1),
        Duration::hours(24),
    ] {
        assert_eq!(refused(&access, sent + age), 1, "{age}");
    }
    let past = access.snapshot_at(sent + Duration::hours(24) + Duration::minutes(1));
    assert!(past.refusals.is_empty());
}

#[test]
fn refusals_leave_the_window_a_minute_at_a_time() {
    let access = RemoteAccess::default();
    access.record_refused_at(RefusalReason::NoIdentity, None, at(10, 58, 30));
    access.record_refused_at(RefusalReason::NoIdentity, None, at(10, 59, 30));
    let counts: Vec<u64> = [(10, 58, 59), (10, 59, 0), (10, 59, 59), (11, 0, 0)]
        .into_iter()
        .map(|(h, m, s)| refused(&access, at(h, m, s) + Duration::days(1)))
        .collect();
    // Each refusal outlasts its 24 hours by less than one minute.
    assert_eq!(counts, [2, 1, 1, 0]);
}

#[test]
fn refusals_merged_past_the_key_cap_keep_their_own_window() {
    let access = RemoteAccess::default();
    let early = at(10, 59, 0);
    for n in 0..MAX_REFUSAL_KEYS {
        let login = format!("u{n}");
        access.record_refused_at(RefusalReason::UnlistedLogin, Some(&login), early);
    }
    // One more login folds an evicted key into the unlisted aggregate.
    let later = early + Duration::hours(23) + Duration::minutes(30);
    access.record_refused_at(RefusalReason::UnlistedLogin, Some("extra"), later);

    let inside = early + Duration::hours(24);
    assert_eq!(refused(&access, inside), MAX_REFUSAL_KEYS as u64 + 1);
    let outside = inside + Duration::minutes(1);
    assert_eq!(refused(&access, outside), 1);
}

#[test]
fn active_devices_follow_the_request_window_and_open_streams() {
    let access = Arc::new(RemoteAccess::default());
    let sent = at(10, 0, 59);
    access.record_admitted_at(ip(1), "a@example.com", sent);
    access.record_admitted_at(ip(2), "b@example.com", sent);
    // The second device keeps a stream open; the first only sent a request.
    let Some(_stream) = access.open_stream(ip(2), "b@example.com") else {
        panic!("opening the stream for the active-device window test should succeed");
    };

    let edge = access.snapshot_at(sent + Duration::seconds(15 * 60));
    assert_eq!(edge.active_devices(), 2);

    let past = access.snapshot_at(sent + Duration::seconds(15 * 60 + 1));
    assert_eq!(past.active_devices(), 1);
    let Some(streaming) = past.devices.iter().find(|d| d.open_streams > 0) else {
        panic!("the open stream should keep its device active");
    };
    assert_eq!(streaming.requests_15m, 0);
    assert_eq!(streaming.login, "b@example.com");
}
