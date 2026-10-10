//! Public provider usage-limit boundary: the reset instant a provider's text
//! names, read across the year boundary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use chrono::{FixedOffset, TimeZone, Utc};
use orbit_agent::{provider_usage_limit, provider_usage_limit_details};

#[test]
fn january_reset_seen_in_late_december_holds_until_next_january() {
    let now = Utc.with_ymd_and_hms(2026, 12, 28, 10, 0, 0).unwrap();
    let text = "You've hit your limit · resets Jan 2, 3am (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // 3am PST on 2027-01-02 is 11:00 UTC.
    let expected = Utc.with_ymd_and_hms(2027, 1, 2, 11, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn explicit_reset_already_passed_today_rolls_to_tomorrow_not_next_year() {
    // 13:00 PDT on 2026-10-08; the 3am PDT reset passed ten hours ago.
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let text = "You've hit your limit · resets Oct 8, 3am (America/Los_Angeles)";

    let limit = provider_usage_limit_details(text, now, utc());

    // 3am PDT on 2026-10-09 is 10:00 UTC: the same-day reset that has passed
    // is tomorrow's, and it must not be pushed a year ahead.
    let expected = Utc.with_ymd_and_hms(2026, 10, 9, 10, 0, 0).unwrap();
    assert_eq!(limit.resets_at, Some(expected));
}

#[test]
fn multibyte_text_before_the_zone_holds_by_default_without_panicking() {
    // `1é2` is four bytes, so the two-byte meridiem split lands inside `é`.
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let text = "You've hit your limit · resets 1é2 (America/Los_Angeles)";

    assert!(provider_usage_limit(text));
    let limit = provider_usage_limit_details(text, now, utc());

    assert_eq!(limit.resets_at, None);
}

#[test]
fn oversized_relative_durations_do_not_panic_and_yield_no_reset() {
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();
    let cases = [
        // Out-of-range DateTime addition (overflows DateTime range)
        "Usage limit reached. Try again in 100000000 days.",
        // Out-of-range TimeDelta units (try_* returns None)
        "Usage limit reached. Resets in 99999999999999d",
        "Usage limit reached. Resets in 99999999999999 hours",
        "Usage limit reached. Resets in 999999999999999 minutes",
        "Usage limit reached. Resets in 9999999999999999 seconds",
        // Digits exceeding i64 range
        "Usage limit reached. Resets in 999999999999999999999999999999999999999999999999999999999999d",
        // Multi-unit duration whose sum overflows TimeDelta
        "Usage limit reached. Resets in 99999999999 days 99999999999 days",
    ];

    for text in cases {
        assert!(provider_usage_limit(text), "should detect limit in {text}");
        let limit = provider_usage_limit_details(text, now, utc());
        assert_eq!(
            limit.resets_at, None,
            "oversized duration in '{text}' should report no reset"
        );
    }
}

#[test]
fn well_formed_relative_resets_resolve_against_reference_time() {
    let now = Utc.with_ymd_and_hms(2026, 10, 8, 20, 0, 0).unwrap();

    let text1 = "Individual quota reached. Resets in 1h37m37s.";
    assert!(provider_usage_limit(text1));
    let limit1 = provider_usage_limit_details(text1, now, utc());
    let expected1 = Utc.with_ymd_and_hms(2026, 10, 8, 21, 37, 37).unwrap();
    assert_eq!(limit1.resets_at, Some(expected1));

    let text2 = "Usage limit reached. Try again in 5 minutes.";
    assert!(provider_usage_limit(text2));
    let limit2 = provider_usage_limit_details(text2, now, utc());
    let expected2 = Utc.with_ymd_and_hms(2026, 10, 8, 20, 5, 0).unwrap();
    assert_eq!(limit2.resets_at, Some(expected2));
}

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}
