#![allow(clippy::unwrap_used)]

//! Load-time condition checks and their evaluation, through the public
//! `check_condition` and `ConditionEvaluator` boundaries.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_automation::condition::{
    BoardFacts, ConditionCarrier, ConditionEvaluator, ConditionFacts, ConditionReads, SelfFacts,
    Truth, check_condition,
};
use orbit_types::workflow::automation::ConditionTrigger;

fn task_condition(when: &str) -> ConditionTrigger {
    serde_json::from_value(serde_json::json!({"scope": "task", "when": when})).unwrap()
}

fn workspace_condition(when: &str) -> ConditionTrigger {
    serde_json::from_value(serde_json::json!({"scope": "workspace", "when": when})).unwrap()
}

/// Facts for a healthy workspace: an empty eligible queue, no open runs and no
/// drain running. Like the explain gatherer, it binds only the board fields the
/// checked condition reads, and `open_runs` and `drain_running` load together.
fn healthy_board_facts(reads: &ConditionReads) -> ConditionFacts {
    let mut board = BoardFacts::default();
    if reads.board.contains("eligible") {
        board.eligible = Some(0);
    }
    if reads.board.contains("open_runs") || reads.board.contains("drain_running") {
        board.open_runs = Some(0);
        board.drain_running = Some(false);
    }
    ConditionFacts {
        now: Utc::now(),
        board,
        tasks: BTreeMap::new(),
        self_facts: SelfFacts::default(),
    }
}

#[test]
fn contradictory_status_prefilter_is_refused() {
    // No status satisfies these conjuncts. Loading them would leave the
    // prefilter empty, which lists every task as a subject instead of none.
    for when in [
        "task.status == 'done' && task.status == 'blocked'",
        "task.status in [] && 'release' in task.tags",
    ] {
        let result = check_condition(&task_condition(when), ConditionCarrier::Routine);
        assert!(
            result.is_err(),
            "accepted a prefilter no task can satisfy: {when}"
        );
    }
}

#[test]
fn satisfiable_status_prefilters_still_load() {
    for when in [
        "task.status == 'done' && task.status in ['done', 'blocked'] && 'release' in task.tags",
        "'release' in task.tags && task.status != 'done'",
    ] {
        let result = check_condition(&task_condition(when), ConditionCarrier::Routine);
        assert!(result.is_ok(), "refused a satisfiable prefilter: {when}");
    }
}

#[test]
fn board_presence_tests_record_their_field_as_a_read() {
    for (when, field) in [
        ("has(board.eligible)", "eligible"),
        ("has(board.open_runs)", "open_runs"),
        ("has(board.drain_running)", "drain_running"),
    ] {
        let checked = check_condition(&workspace_condition(when), ConditionCarrier::Routine)
            .unwrap_or_else(|error| panic!("refused {when}: {error}"));
        assert!(
            checked.reads.board.contains(field),
            "{when} did not record board.{field} as a read"
        );
    }
}

#[test]
fn board_presence_test_matches_direct_read_on_healthy_facts() {
    // Each direct read is true on healthy facts, so its presence-only form must
    // be true too. Before the fix the presence form read as false: the field was
    // never loaded, so `has()` found no fact.
    for (presence, direct) in [
        (
            "has(board.eligible)",
            "has(board.eligible) && board.eligible.count >= 0",
        ),
        (
            "has(board.open_runs)",
            "has(board.open_runs) && board.open_runs.count >= 0",
        ),
        (
            "has(board.drain_running)",
            "has(board.drain_running) && !board.drain_running",
        ),
    ] {
        let direct_condition =
            check_condition(&workspace_condition(direct), ConditionCarrier::Routine).unwrap();
        let presence_condition =
            check_condition(&workspace_condition(presence), ConditionCarrier::Routine).unwrap();
        let expected = ConditionEvaluator::new(&healthy_board_facts(&direct_condition.reads))
            .truth(&direct_condition.when, None);
        let actual = ConditionEvaluator::new(&healthy_board_facts(&presence_condition.reads))
            .truth(&presence_condition.when, None);
        assert_eq!(
            expected,
            Truth::True,
            "direct read is not true on healthy facts: {direct}"
        );
        assert_eq!(
            actual, expected,
            "{presence} differs from its direct-read form on healthy facts"
        );
    }
}

#[test]
fn board_status_map_reads_require_stored_status_spellings() {
    for when in [
        "board.by_status.typo == 0",
        "board.by_status['typo'] == 0",
        "has(board.by_status.typo)",
    ] {
        let error = match check_condition(&workspace_condition(when), ConditionCarrier::Routine) {
            Ok(_) => panic!("accepted unknown board status key: {when}"),
            Err(error) => error,
        };
        assert_eq!(
            error.clause, "trigger.condition.when",
            "unknown status key was reported against the wrong clause: {when}"
        );
        assert!(
            error.position.is_some(),
            "unknown status key had no source position: {when}"
        );
        assert!(
            error.message.contains("unknown task status 'typo'"),
            "unknown status key had no status-spelling error: {when}: {error}"
        );
    }

    for when in [
        "board.by_status.in_progress == 0",
        "board.by_status['in_progress'] == 0",
        "has(board.by_status.in_progress)",
    ] {
        check_condition(&workspace_condition(when), ConditionCarrier::Routine)
            .unwrap_or_else(|error| panic!("refused stored status key in {when}: {error}"));
    }
}

#[test]
fn duration_negation_is_refused_at_load() {
    // cel 0.15 has no duration negation overload, so the checker must refuse
    // unary '-' on a duration at load instead of accepting an expression that
    // can only evaluate to unknown. The `true || ...` form pins the absorbed
    // case: cel's `||` would return true despite the missing overload.
    for carrier in [ConditionCarrier::Routine, ConditionCarrier::AutoTask] {
        for when in [
            "-duration('1h') < duration('0s')",
            "true || -duration('1h') < duration('0s')",
        ] {
            let error = match check_condition(&workspace_condition(when), carrier) {
                Ok(_) => panic!("accepted duration negation for {carrier:?}: {when}"),
                Err(error) => error,
            };
            assert_eq!(
                error.clause,
                format!("{}.when", carrier.clause()),
                "wrong clause for {when}"
            );
            assert!(
                error.position.is_some(),
                "missing source position for {when}: {error}"
            );
            assert!(
                error.message.contains("cannot negate duration"),
                "unexpected message for {when}: {error}"
            );
        }
    }
}

#[test]
fn supported_negation_still_checks_and_evaluates() {
    // Numeric negation and signed duration string literals are supported and
    // must keep loading and evaluating.
    for carrier in [ConditionCarrier::Routine, ConditionCarrier::AutoTask] {
        for when in ["-1 < 0", "duration('-1h') < duration('0s')"] {
            let checked = check_condition(&workspace_condition(when), carrier)
                .unwrap_or_else(|error| panic!("refused {when} for {carrier:?}: {error}"));
            let truth = ConditionEvaluator::new(&healthy_board_facts(&checked.reads))
                .truth(&checked.when, None);
            assert_eq!(truth, Truth::True, "{when} did not evaluate true");
        }
    }
}
