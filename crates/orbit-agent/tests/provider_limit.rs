//! Public provider usage-limit boundary: the reset instant a provider's text
//! names, read across the year boundary.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use chrono::{FixedOffset, TimeZone, Utc};
use orbit_agent::provider_usage_limit_details;

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

fn utc() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}
