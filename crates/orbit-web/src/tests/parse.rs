use chrono::Utc;

use super::super::parse::*;

#[test]
fn parses_rfc3339() {
    // test-only: unwrap is acceptable for deterministic RFC3339 input in an isolated unit test
    #[allow(clippy::unwrap_used)]
    let ts = parse_since("2025-01-01T00:00:00Z").unwrap();
    assert_eq!(ts.timestamp(), 1735689600);
}

#[test]
fn parses_duration() {
    // This is a bit racy on "now", but we can assert it's recent.
    // test-only: unwrap is acceptable for deterministic duration input in an isolated unit test
    #[allow(clippy::unwrap_used)]
    let ts = parse_since("10s").unwrap();
    let now = Utc::now();
    assert!(now.signed_duration_since(ts).num_seconds() >= 9);
}

#[test]
fn bare_numbers_are_seconds() {
    assert_eq!(parse_duration_seconds("90").expect("bare seconds"), 90);
    assert_eq!(parse_duration_seconds("2m").expect("minutes"), 120);
}

#[test]
fn overflowing_durations_are_rejected_rather_than_wrapped() {
    let error = parse_duration_seconds("99999999999999999w").expect_err("must not wrap");
    assert!(matches!(error, orbit_core::OrbitError::InvalidInput(_)));
}
