use super::super::due::{
    DueDecision, NATURAL_SLOT_GRACE_SECONDS, due_decision_with_grace, parse_cron,
    truncate_to_minute,
};
use chrono::{Duration, TimeZone};
use chrono_tz::Europe::Berlin;
use orbit_types::workflow::MissedRunPolicy;

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
