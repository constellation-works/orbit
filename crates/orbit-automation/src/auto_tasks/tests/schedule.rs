//! Due-math tests [ORB-10149]: cron + interval, natural fire, not-due, and
//! catch-up collapse — plus the forward projection, which deliberately does
//! not agree with a pending catch-up decision.

use chrono::{DateTime, Duration, TimeZone, Utc};
use orbit_types::workflow::{AutoTaskSchedule, MAX_AUTO_TASK_INTERVAL_MINUTES};

use crate::auto_tasks::schedule::{
    AutoTaskDueDecision, decide_due, next_scheduled_slot, validate_schedule,
};

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

/// Every minute in every host-local zone, so slot assertions stay deterministic
/// wherever the suite runs.
fn every_minute_cron() -> AutoTaskSchedule {
    AutoTaskSchedule::Cron {
        cron: "* * * * *".to_string(),
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
fn interval_never_fires_for_a_now_before_the_baseline() {
    let baseline = at(2026, 1, 1, 12, 0);
    // A backwards host clock must not manufacture a boundary behind the anchor.
    let decision = decide_due(
        &interval(60),
        baseline,
        None,
        baseline - Duration::minutes(90),
    )
    .expect("decide");
    assert_eq!(decision, AutoTaskDueDecision::NotDue);
}

#[test]
fn interval_projection_skips_the_boundary_now_falls_in() {
    let baseline = at(2026, 1, 1, 0, 0);

    // Exactly on a boundary: that boundary has already been offered, so the
    // projection is the following one.
    assert_eq!(
        next_scheduled_slot(
            &interval(60),
            Some(baseline),
            baseline + Duration::minutes(60)
        )
        .expect("project"),
        Some(baseline + Duration::minutes(120))
    );
    // Mid-period: still the end of the period `now` sits in.
    assert_eq!(
        next_scheduled_slot(
            &interval(60),
            Some(baseline),
            baseline + Duration::minutes(90)
        )
        .expect("project"),
        Some(baseline + Duration::minutes(120))
    );
}

#[test]
fn interval_projection_before_the_baseline_is_the_first_firing_boundary() {
    let baseline = at(2026, 1, 1, 12, 0);
    // The baseline boundary is the exclusive due floor and never fires, so the
    // first occurrence a caller can wait for is one period past it.
    assert_eq!(
        next_scheduled_slot(
            &interval(30),
            Some(baseline),
            baseline - Duration::minutes(90)
        )
        .expect("project"),
        Some(baseline + Duration::minutes(30))
    );
}

#[test]
fn interval_projection_looks_forward_while_a_missed_slot_is_still_due() {
    let baseline = at(2026, 1, 1, 0, 0);
    // Six hours of downtime: the scheduler still owes the +360m boundary.
    let now = baseline + Duration::minutes(370);

    let due = decide_due(&interval(60), baseline, None, now).expect("decide");
    let projected = next_scheduled_slot(&interval(60), Some(baseline), now)
        .expect("project")
        .expect("interval projects with a baseline");

    assert_eq!(
        due,
        AutoTaskDueDecision::Fire {
            slot: (baseline + Duration::minutes(360)).to_rfc3339(),
        }
    );
    assert_eq!(projected, baseline + Duration::minutes(420));
    assert!(
        projected > now,
        "the projection is the next arrival, not the pending catch-up slot"
    );
    assert_ne!(
        AutoTaskDueDecision::Fire {
            slot: projected.to_rfc3339(),
        },
        due,
        "a future projection must not be asserted equal to an owed catch-up slot"
    );
}

#[test]
fn interval_projection_requires_a_baseline_anchor() {
    // Interval slots are anchored at registration; without a cursor there is
    // nothing to anchor to, and the caller labels the row never-observed.
    assert_eq!(
        next_scheduled_slot(&interval(60), None, at(2026, 1, 1, 0, 0)).expect("project"),
        None
    );
}

#[test]
fn validate_schedule_rejects_bad_cron_and_out_of_range_intervals() {
    assert!(
        validate_schedule(&AutoTaskSchedule::Cron {
            cron: "not a cron".to_string(),
        })
        .is_err()
    );
    assert!(validate_schedule(&interval(0)).is_err());
    assert!(validate_schedule(&interval(u64::MAX)).is_err());
    assert!(validate_schedule(&interval(30)).is_ok());
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

#[test]
fn interval_slot_near_the_date_range_edge_errors_instead_of_wrapping() {
    let baseline = DateTime::<Utc>::MAX_UTC - Duration::minutes(5);

    // A cursor anchored at the end of representable time cannot produce a
    // wrapped slot that the scheduler would mistake for a due boundary.
    assert!(
        next_scheduled_slot(
            &interval(MAX_AUTO_TASK_INTERVAL_MINUTES),
            Some(baseline),
            baseline
        )
        .is_err()
    );
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
fn cron_slots_are_minute_pinned_regardless_of_sub_minute_now() {
    let baseline = at(2026, 7, 2, 21, 59);
    // "now" carries seconds and nanos, as real scheduler passes do; the slot
    // that becomes the idempotency key must not.
    let now = Utc
        .with_ymd_and_hms(2026, 7, 2, 22, 0, 42)
        .single()
        .expect("valid ts")
        + Duration::nanoseconds(785_766_897);

    let decision = decide_due(&every_minute_cron(), baseline, None, now).expect("decide");
    assert_eq!(
        decision,
        AutoTaskDueDecision::Fire {
            slot: at(2026, 7, 2, 22, 0).to_rfc3339(),
        }
    );

    let projected = next_scheduled_slot(&every_minute_cron(), None, now)
        .expect("project")
        .expect("cron projects without a cursor");
    assert_eq!(projected, at(2026, 7, 2, 22, 1));
    assert!(projected > now, "the projection must stay ahead of now");
}

#[test]
fn cron_does_not_refire_a_consumed_slot() {
    let consumed = at(2026, 7, 2, 22, 0);
    // The pass re-runs 30 seconds later with the cursor already on this slot.
    let now = consumed + Duration::seconds(30);
    let decision = decide_due(&every_minute_cron(), consumed, Some(consumed), now).expect("decide");
    assert_eq!(decision, AutoTaskDueDecision::NotDue);
}

#[test]
fn cron_projection_looks_forward_while_a_missed_slot_is_still_due() {
    let baseline = at(2026, 7, 2, 22, 0);
    // Well past the natural-slot grace: the scheduler owes a catch-up fire for
    // the 23:30 slot while the schedule's next arrival is 23:31.
    let now = at(2026, 7, 2, 23, 30) + Duration::seconds(30);

    let due = decide_due(&every_minute_cron(), baseline, None, now).expect("decide");
    let projected = next_scheduled_slot(&every_minute_cron(), None, now)
        .expect("project")
        .expect("cron projects without a cursor");

    assert_eq!(
        due,
        AutoTaskDueDecision::Fire {
            slot: at(2026, 7, 2, 23, 30).to_rfc3339(),
        }
    );
    assert_eq!(projected, at(2026, 7, 2, 23, 31));
    assert!(projected > now);
}

#[test]
fn delivery_schedules_have_no_time_based_projection_or_due_decision() {
    let schedule = AutoTaskSchedule::Deliveries {
        deliveries_landed: orbit_types::workflow::automation::DeliveryTrigger {
            owner_machine: None,
            branch: "agent-main".to_string(),
            threshold: 3,
            max_wait_minutes: 60,
            coverage: orbit_types::workflow::automation::CoverageClass::IntegratedQaV1,
            max_items: 20,
            retries: 0,
        },
    };
    let now = at(2026, 1, 1, 0, 0);

    assert_eq!(
        next_scheduled_slot(&schedule, Some(now), now).expect("project"),
        None
    );
    assert!(
        decide_due(&schedule, now, None, now).is_err(),
        "delivery schedules are decided against their source, not the clock"
    );
}
