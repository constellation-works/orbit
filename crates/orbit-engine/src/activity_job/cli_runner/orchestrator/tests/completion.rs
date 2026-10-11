use std::time::Duration;

use super::super::completion::print_budget_exhausted;

// Combinatorial budget/exit/envelope logic: exact hour-long boundaries cannot
// be exercised economically by a real provider process. [ORB-15206]
#[test]
fn only_a_clean_exit_without_an_envelope_at_the_injected_budget_is_a_timeout() {
    let mut argv = Vec::new();
    let budget = orbit_agent::apply_antigravity_print_timeout(
        "antigravity",
        &mut argv,
        Duration::from_secs(3600),
    );
    assert_eq!(budget, Some(Duration::from_secs(3570)));
    for (exit_success, missing, budget, elapsed_ms, expected) in [
        (true, true, budget, 3_569_999, false),
        (true, true, budget, 3_570_000, true),
        (true, true, budget, 3_570_001, true),
        (true, false, budget, 3_570_000, false),
        (false, true, budget, 3_570_000, false),
        (true, true, None, 3_570_000, false),
    ] {
        assert_eq!(
            print_budget_exhausted(
                exit_success,
                missing,
                budget,
                Duration::from_millis(elapsed_ms),
            ),
            expected,
            "ORB-15206: exit_success={exit_success}, missing={missing}, budget={budget:?}, elapsed={elapsed_ms}ms",
        );
    }
}
