#![allow(clippy::unwrap_used)]

//! Load-time condition checks, through the public `check_condition` boundary.

use orbit_automation::condition::{ConditionCarrier, check_condition};
use orbit_types::workflow::automation::ConditionTrigger;

fn task_condition(when: &str) -> ConditionTrigger {
    serde_json::from_value(serde_json::json!({"scope": "task", "when": when})).unwrap()
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
