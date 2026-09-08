use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Europe::Berlin;

use super::super::due::{
    DueDecision, NATURAL_SLOT_GRACE_SECONDS, due_decision, due_decision_with_grace,
    natural_slot_grace_for_cadence, next_occurrence, parse_cron, truncate_to_minute,
};
use orbit_types::workflow::MissedRunPolicy;

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
        .single()
        .expect("valid ts")
}

#[test]
fn parse_cron_accepts_standard_five_field_and_rejects_garbage() {
    parse_cron("0 22 * * *").expect("nightly");
    parse_cron("*/30 * * * *").expect("half-hourly");
    parse_cron("not a cron").expect_err("garbage must fail");
    parse_cron("").expect_err("empty must fail");
}

#[test]
fn not_due_when_no_slot_after_lower_bound() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    // Last fired at yesterday's 22:00 slot; now is 10:00 — nothing new.
    let lower = at(2026, 7, 1, 22, 0, 0);
    let now = at(2026, 7, 2, 10, 0, 0);
    let decision = due_decision(&cron, MissedRunPolicy::Skip, &lower, &now).expect("decision");
    assert_eq!(decision, DueDecision::NotDue);
}

#[test]
fn natural_slot_fires_within_grace() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    let lower = at(2026, 7, 1, 22, 0, 0);
    // Sweep runs 40 seconds after the slot — natural fire.
    let now = at(2026, 7, 2, 22, 0, 40);
    let decision = due_decision(&cron, MissedRunPolicy::Skip, &lower, &now).expect("decision");
    assert_eq!(
        decision,
        DueDecision::Fire {
            slot: at(2026, 7, 2, 22, 0, 0),
            is_catch_up: false,
        }
    );
}

#[test]
fn default_cadence_keeps_its_existing_two_minute_natural_window() {
    let cron = parse_cron("0 * * * *").expect("cron");
    let lower = at(2026, 7, 2, 21, 0, 0);

    let final_natural = due_decision(
        &cron,
        MissedRunPolicy::Skip,
        &lower,
        &at(2026, 7, 2, 22, 2, 0),
    )
    .expect("decision");
    assert!(matches!(
        final_natural,
        DueDecision::Fire {
            is_catch_up: false,
            ..
        }
    ));

    let first_missed = due_decision(
        &cron,
        MissedRunPolicy::Skip,
        &lower,
        &at(2026, 7, 2, 22, 2, 1),
    )
    .expect("decision");
    assert_eq!(first_missed, DueDecision::NotDue);
    assert_eq!(NATURAL_SLOT_GRACE_SECONDS, 120);
}

#[test]
fn configured_cadence_keeps_the_incident_slot_natural_once() {
    let cron = parse_cron("5 * * * *").expect("cron");
    let lower = at(2026, 9, 7, 0, 5, 0);
    let grace = natural_slot_grace_for_cadence(300).expect("five-minute grace");

    // The production incident polled before the 01:05 slot, then again at
    // 01:08:43. The 223-second-old slot is still an ordinary fire, not a
    // catch-up, under the configured five-minute clock.
    let legacy_decision = due_decision(
        &cron,
        MissedRunPolicy::Skip,
        &lower,
        &at(2026, 9, 7, 1, 8, 43),
    )
    .expect("legacy decision");
    assert_eq!(legacy_decision, DueDecision::NotDue);

    let decision = due_decision_with_grace(
        &cron,
        MissedRunPolicy::Skip,
        &lower,
        &at(2026, 9, 7, 1, 8, 43),
        grace,
    )
    .expect("decision");
    assert_eq!(
        decision,
        DueDecision::Fire {
            slot: at(2026, 9, 7, 1, 5, 0),
            is_catch_up: false,
        }
    );
}

#[test]
fn cadence_grace_handles_poll_phase_without_masking_real_downtime() {
    let cron = parse_cron("5 * * * *").expect("cron");
    let lower = at(2026, 9, 7, 0, 5, 0);
    let grace = natural_slot_grace_for_cadence(300).expect("five-minute grace");

    for now in [at(2026, 9, 7, 1, 5, 5), at(2026, 9, 7, 1, 8, 43)] {
        let decision = due_decision_with_grace(&cron, MissedRunPolicy::Skip, &lower, &now, grace)
            .expect("decision");
        assert!(matches!(
            decision,
            DueDecision::Fire {
                is_catch_up: false,
                ..
            }
        ));
    }

    let after_one_delayed_poll = due_decision_with_grace(
        &cron,
        MissedRunPolicy::Skip,
        &lower,
        &at(2026, 9, 7, 1, 15, 1),
        grace,
    )
    .expect("decision");
    assert_eq!(after_one_delayed_poll, DueDecision::NotDue);

    let catch_up = due_decision_with_grace(
        &cron,
        MissedRunPolicy::CatchUpOnce,
        &lower,
        &at(2026, 9, 7, 1, 15, 1),
        grace,
    )
    .expect("decision");
    assert_eq!(
        catch_up,
        DueDecision::Fire {
            slot: at(2026, 9, 7, 1, 5, 0),
            is_catch_up: true,
        }
    );
}

#[test]
fn catch_up_once_collapses_a_week_of_missed_slots_into_one_fire() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    // Laptop slept for a week after the July 1 fire.
    let lower = at(2026, 7, 1, 22, 0, 0);
    let now = at(2026, 7, 8, 9, 30, 0);
    let decision =
        due_decision(&cron, MissedRunPolicy::CatchUpOnce, &lower, &now).expect("decision");
    // One make-up fire for the *latest* missed slot, not seven fires.
    assert_eq!(
        decision,
        DueDecision::Fire {
            slot: at(2026, 7, 7, 22, 0, 0),
            is_catch_up: true,
        }
    );
}

#[test]
fn skip_waits_for_the_next_natural_slot() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    let lower = at(2026, 7, 1, 22, 0, 0);
    let now = at(2026, 7, 8, 9, 30, 0);
    let decision = due_decision(&cron, MissedRunPolicy::Skip, &lower, &now).expect("decision");
    assert_eq!(decision, DueDecision::NotDue);
}

#[test]
fn slots_are_minute_aligned_regardless_of_sub_minute_now() {
    let cron = parse_cron("* * * * *").expect("cron");
    let lower = at(2026, 7, 2, 21, 59, 0);
    // "now" carries seconds + nanos, as real sweeps do; the slot must not.
    let now = Utc
        .with_ymd_and_hms(2026, 7, 2, 22, 0, 42)
        .single()
        .expect("valid ts")
        + chrono::Duration::nanoseconds(785_766_897);
    let decision = due_decision(&cron, MissedRunPolicy::Skip, &lower, &now).expect("decision");
    assert_eq!(
        decision,
        DueDecision::Fire {
            slot: at(2026, 7, 2, 22, 0, 0),
            is_catch_up: false,
        }
    );
}

#[test]
fn dst_fold_slots_are_distinct_and_due_decisions_do_not_error() {
    let cron = parse_cron("30 2 * * *").expect("cron");
    let earliest = Berlin
        .with_ymd_and_hms(2025, 10, 26, 2, 30, 0)
        .earliest()
        .expect("DST fold has an earliest occurrence");
    let latest = Berlin
        .with_ymd_and_hms(2025, 10, 26, 2, 30, 0)
        .latest()
        .expect("DST fold has a latest occurrence");

    let first_decision = due_decision_with_grace(
        &cron,
        MissedRunPolicy::Skip,
        &(earliest - Duration::minutes(1)),
        &(earliest + Duration::seconds(45)),
        Duration::seconds(NATURAL_SLOT_GRACE_SECONDS),
    )
    .expect("first fold occurrence is due");
    let second_decision = due_decision_with_grace(
        &cron,
        MissedRunPolicy::Skip,
        &earliest,
        &(latest + Duration::seconds(45)),
        Duration::seconds(NATURAL_SLOT_GRACE_SECONDS),
    )
    .expect("second fold occurrence is evaluated");

    assert_eq!(
        first_decision,
        DueDecision::Fire {
            slot: earliest,
            is_catch_up: false,
        }
    );
    assert_eq!(second_decision, DueDecision::NotDue);

    let earliest_slot = truncate_to_minute(earliest + Duration::seconds(45));
    let latest_slot = truncate_to_minute(latest + Duration::seconds(45));
    assert_eq!(earliest_slot, earliest);
    assert_eq!(latest_slot, latest);
    assert_ne!(earliest_slot, latest_slot);
}

#[test]
fn slot_exactly_at_lower_bound_does_not_refire() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    let slot = at(2026, 7, 2, 22, 0, 0);
    // Sweep re-runs 30 seconds later with the cursor already at this slot.
    let now = at(2026, 7, 2, 22, 0, 30);
    let decision =
        due_decision(&cron, MissedRunPolicy::CatchUpOnce, &slot, &now).expect("decision");
    assert_eq!(decision, DueDecision::NotDue);
}

#[test]
fn baseline_in_the_future_of_all_slots_suppresses_firing() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    // Routine first observed at 23:00 — the 22:00 slot predates registration.
    let baseline = at(2026, 7, 2, 23, 0, 0);
    let now = at(2026, 7, 2, 23, 30, 0);
    let decision =
        due_decision(&cron, MissedRunPolicy::CatchUpOnce, &baseline, &now).expect("decision");
    assert_eq!(decision, DueDecision::NotDue);
}

#[test]
fn next_occurrence_is_minute_pinned_and_strictly_after_a_sub_minute_now() {
    let cron = parse_cron("* * * * *").expect("cron");
    let now = at(2026, 7, 2, 22, 0, 42) + Duration::nanoseconds(785_766_897);

    let next = next_occurrence(&cron, &now).expect("projection");

    assert_eq!(next, at(2026, 7, 2, 22, 1, 0));
    assert!(
        next > now,
        "pinning the occurrence to its minute must not pull it back to now"
    );
}

#[test]
fn next_occurrence_is_stable_across_polls_within_one_minute() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    let first = next_occurrence(&cron, &at(2026, 7, 2, 10, 0, 3)).expect("first poll");
    let second = next_occurrence(&cron, &at(2026, 7, 2, 10, 0, 47)).expect("second poll");

    assert_eq!(first, at(2026, 7, 2, 22, 0, 0));
    assert_eq!(first, second);
}

#[test]
fn next_occurrence_looks_forward_while_a_missed_slot_is_still_due() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    // A week of laptop sleep after the July 1 fire.
    let lower = at(2026, 7, 1, 22, 0, 0);
    let now = at(2026, 7, 8, 9, 30, 0);

    let due = due_decision(&cron, MissedRunPolicy::CatchUpOnce, &lower, &now).expect("decision");
    let projected = next_occurrence(&cron, &now).expect("projection");

    // Catch-up eligibility names the latest missed slot, already in the past.
    assert_eq!(
        due,
        DueDecision::Fire {
            slot: at(2026, 7, 7, 22, 0, 0),
            is_catch_up: true,
        }
    );
    // The projection names the schedule's next arrival, always ahead of now.
    assert_eq!(projected, at(2026, 7, 8, 22, 0, 0));
    assert!(projected > now);
}

#[test]
fn next_occurrence_reports_the_same_upcoming_slot_under_either_missed_run_policy() {
    let cron = parse_cron("0 22 * * *").expect("cron");
    let lower = at(2026, 7, 1, 22, 0, 0);
    let now = at(2026, 7, 8, 9, 30, 0);

    // `skip` declines the missed slot entirely; the projection is unchanged,
    // because a projection is not a policy decision.
    let skipped = due_decision(&cron, MissedRunPolicy::Skip, &lower, &now).expect("decision");

    assert_eq!(skipped, DueDecision::NotDue);
    assert_eq!(
        next_occurrence(&cron, &now).expect("projection"),
        at(2026, 7, 8, 22, 0, 0)
    );
}
