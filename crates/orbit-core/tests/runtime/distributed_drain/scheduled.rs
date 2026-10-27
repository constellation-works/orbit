//! A task's `not_before` holds it on the owner's pull admission, so no
//! follower claims it before its time.

use super::*;

const FUTURE: &str = "2099-01-01T09:00:00Z";
const PAST: &str = "2020-01-01T09:00:00Z";

/// The owner hands a follower the unscheduled task and keeps the one
/// scheduled ahead; the follower's idle pass records it waiting `scheduled`
/// with its time. Once its time has passed, the next drain claims it with no
/// operator action.
fn a_scheduled_task_is_claimed_by_no_follower_before_its_time() {
    let pair = Pair::new(2);
    let (scheduled, plain) = (pair.tasks[0].clone(), pair.tasks[1].clone());
    let schedule = |pair: &Pair, time: &str| {
        pair.wire
            .owner
            .run_tool(
                "orbit.task.update",
                json!({"id": scheduled, "not_before": time, "model": "codex"}),
            )
            .expect("schedule owner task");
    };
    schedule(&pair, FUTURE);

    let drain = pair.start_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), plain);
    pair.leaf_fails_with(&leaf, "candidate validation failed");
    let idle = pair.pass(&drain);
    assert_eq!(idle["admitted"], 0, "{idle}");
    assert_eq!(pair.owner_status(&scheduled), "backlog");
    assert!(!pair.owner_has_claim_for_task(&scheduled));
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("the idle pass is recorded");
    let wait = recorded
        .deferred
        .iter()
        .find(|entry| entry.task_id == scheduled)
        .unwrap_or_else(|| panic!("{scheduled} is deferred: {recorded:#?}"));
    assert_eq!(wait.reason.as_deref(), Some("scheduled"), "{wait:?}");
    assert_eq!(
        wait.detail.as_deref(),
        Some(
            format!(
                "scheduled: not before {FUTURE}; the hold lifts on the first admission pass \
                 after that time"
            )
            .as_str()
        ),
        "{wait:?}"
    );

    schedule(&pair, PAST);
    let due = pair.start_drain();
    let due_leaf = pair.running_leaf(&due, 1);
    assert_eq!(pair.claimed_task(&due_leaf), scheduled);
}

crate::scenario_table!(
    a_scheduled_task_is_claimed_by_no_follower_before_its_time_scenarios,
    a_scheduled_task_is_claimed_by_no_follower_before_its_time
);
