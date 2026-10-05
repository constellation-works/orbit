//! Due-math tests [ORB-10149]: cron + interval, natural fire, not-due, and
//! catch-up collapse — plus the forward projection, which deliberately does
//! not agree with a pending catch-up decision.

use crate::auto_tasks::schedule::{decide_due, next_scheduled_slot};
use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::workflow::{AutoTaskSchedule, MAX_AUTO_TASK_INTERVAL_MINUTES};

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0)
        .single()
        .expect("valid ts")
}

fn interval(minutes: u64) -> AutoTaskSchedule {
    AutoTaskSchedule::Interval {
        every_minutes: minutes,
    }
}

#[test]
fn interval_arithmetic_rejects_out_of_range_values_on_both_consumers() {
    let baseline = at(2026, 1, 1, 0, 0);
    let now = baseline + Duration::minutes(1);

    // Zero, one past the supported maximum, and a value that cannot survive the
    // conversion at all: every one is an error, never an overflow or a panic.
    for every_minutes in [0, MAX_AUTO_TASK_INTERVAL_MINUTES + 1, u64::MAX] {
        assert!(
            decide_due(&interval(every_minutes), baseline, None, now).is_err(),
            "due decision accepted every_minutes={every_minutes}"
        );
        assert!(
            next_scheduled_slot(&interval(every_minutes), Some(baseline), now).is_err(),
            "projection accepted every_minutes={every_minutes}"
        );
    }

    // The supported maximum still computes on both paths.
    assert!(
        decide_due(
            &interval(MAX_AUTO_TASK_INTERVAL_MINUTES),
            baseline,
            None,
            now
        )
        .is_ok()
    );
    assert!(
        next_scheduled_slot(
            &interval(MAX_AUTO_TASK_INTERVAL_MINUTES),
            Some(baseline),
            now
        )
        .is_ok()
    );
}
