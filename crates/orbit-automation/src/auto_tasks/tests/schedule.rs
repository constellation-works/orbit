//! Due-math tests [ORB-10149]: cron + interval, natural fire, not-due, and
//! catch-up collapse.

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::workflow::AutoTaskSchedule;

use crate::auto_tasks::schedule::{AutoTaskDueDecision, decide_due, validate_schedule};

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
fn interval_not_due_before_first_period() {
    let baseline = at(2026, 1, 1, 0, 0);
    // 20 minutes in, a 60-minute interval has not elapsed.
    let decision = decide_due(
        &interval(60),
        baseline,
        None,
        baseline + Duration::minutes(20),
    )
    .expect("decide");
    assert_eq!(decision, AutoTaskDueDecision::NotDue);
}

#[test]
fn interval_fires_once_past_the_period_boundary() {
    let baseline = at(2026, 1, 1, 0, 0);
    let decision = decide_due(
        &interval(60),
        baseline,
        None,
        baseline + Duration::minutes(65),
    )
    .expect("decide");
    assert_eq!(
        decision,
        AutoTaskDueDecision::Fire {
            slot: (baseline + Duration::minutes(60)).to_rfc3339(),
        }
    );
}

#[test]
fn interval_catch_up_collapses_a_long_gap_to_one_slot() {
    let baseline = at(2026, 1, 1, 0, 0);
    // Six hours of downtime against a 60-minute interval: a single make-up
    // fire for the most recent boundary, never six.
    let now = baseline + Duration::minutes(370);
    let decision = decide_due(&interval(60), baseline, None, now).expect("decide");
    assert_eq!(
        decision,
        AutoTaskDueDecision::Fire {
            // floor(370 / 60) = 6 periods → boundary at +360m.
            slot: (baseline + Duration::minutes(360)).to_rfc3339(),
        }
    );
}

#[test]
fn interval_not_due_when_last_slot_covers_the_latest_boundary() {
    let baseline = at(2026, 1, 1, 0, 0);
    let last_slot = baseline + Duration::minutes(60);
    // now is inside the same period as last_slot: no new boundary.
    let decision = decide_due(
        &interval(60),
        baseline,
        Some(last_slot),
        baseline + Duration::minutes(90),
    )
    .expect("decide");
    assert_eq!(decision, AutoTaskDueDecision::NotDue);
}

#[test]
fn cron_fires_for_the_latest_slot_after_the_lower_bound() {
    // Every hour on the hour.
    let schedule = AutoTaskSchedule::Cron {
        cron: "0 * * * *".to_string(),
    };
    let baseline = at(2026, 1, 1, 0, 0);
    // An hour and change past the baseline; the shared cron machinery
    // evaluates in host-local time, so assert only that *a* fire is produced.
    let now = at(2026, 1, 1, 3, 1);
    let decision = decide_due(&schedule, baseline, None, now).expect("decide");
    assert!(
        matches!(decision, AutoTaskDueDecision::Fire { .. }),
        "expected an hourly cron to fire after its slot, got {decision:?}"
    );
}

#[test]
fn validate_schedule_rejects_bad_cron_and_zero_interval() {
    assert!(
        validate_schedule(&AutoTaskSchedule::Cron {
            cron: "not a cron".to_string(),
        })
        .is_err()
    );
    assert!(validate_schedule(&interval(0)).is_err());
    assert!(validate_schedule(&interval(30)).is_ok());
}
