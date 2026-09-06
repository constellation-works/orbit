use chrono::{DateTime, TimeZone, Utc};

use super::super::due::{DueDecision, due_decision, parse_cron};
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
