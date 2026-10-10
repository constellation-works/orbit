use std::time::Duration;

use super::super::completion::print_timeout_spent;

#[test]
fn print_timeout_is_spent_only_at_or_after_the_budget_without_an_envelope() {
    let budget = Duration::from_secs(7170);
    let second = Duration::from_secs(1);
    let cases = [
        ("one second before", budget - second, true, false),
        ("exactly at", budget, true, true),
        ("one second after", budget + second, true, true),
        ("at budget with envelope", budget, false, false),
        ("far past budget with envelope", budget * 2, false, false),
    ];
    for (name, elapsed, envelope_missing, expected) in cases {
        assert_eq!(
            print_timeout_spent(envelope_missing, Some(budget), elapsed),
            expected,
            "{name}"
        );
    }
    // A provider with no injected budget has nothing to spend (ORB-14683).
    assert!(
        !print_timeout_spent(true, None, budget * 10),
        "no injected print-timeout must never classify as a spent budget"
    );
}
