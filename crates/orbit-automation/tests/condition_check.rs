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
