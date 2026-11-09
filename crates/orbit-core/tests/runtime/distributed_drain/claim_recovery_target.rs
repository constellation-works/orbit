//! [ORB-23540] Claim recovery moves the task to any status the lifecycle
//! allows from where the claim holds it, in the one transaction that fences
//! the attempt; `done` keeps the ordinary completion gates.

use super::*;
use orbit_types::task::TaskStatus;

fn claim_id(pair: &Pair, leaf: &str) -> String {
    pair.owner_claim_for_leaf(leaf)["claim"]["claim_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn console_targets(pair: &Pair, claim_id: &str) -> Vec<String> {
    let console = pair.wire.owner.distributed_claim_console().unwrap();
    let row = console["claims"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["claim_id"] == claim_id)
        .cloned()
        .unwrap_or_else(|| panic!("claim {claim_id} in the console: {console:#}"));
    row["recover_targets"]
        .as_array()
        .unwrap_or_else(|| panic!("recover_targets listed: {row:#}"))
        .iter()
        .map(|value| value.as_str().unwrap().to_string())
        .collect()
}

fn recover(
    pair: &Pair,
    claim_id: &str,
    phase: &str,
    status: TaskStatus,
    key: &str,
) -> Result<Value, OrbitError> {
    pair.wire.owner.recover_claim_as_operator(
        claim_id,
        phase,
        status,
        "operator",
        "operator decision after the follower stopped",
        key,
    )
}

/// A running claim's task is in progress, so recovery offers every status
/// the lifecycle allows from there except staying in progress, and one
/// recovery to `archived` fences the attempt and shelves the task.
fn a_running_claim_recovers_to_a_lifecycle_valid_status_beyond_blocked_or_backlog() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let claim = claim_id(&pair, &leaf);
    assert_eq!(
        console_targets(&pair, &claim),
        [
            "blocked", "backlog", "review", "archived", "rejected", "someday"
        ],
        "done is reachable only from review, and in-progress is never a recovery target"
    );

    let recovered = recover(&pair, &claim, "running", TaskStatus::Archived, "archive").unwrap();

    assert_eq!(recovered["task_status"], "archived", "{recovered}");
    assert_eq!(recovered["phase"], "revoked", "{recovered}");
    assert_eq!(pair.owner_status(&task), "archived");
    assert_eq!(pair.owner_claim(&claim)["claim"]["phase"], "revoked");
}

/// A target the lifecycle does not allow is refused with the rule named and
/// changes nothing: the claim keeps running and its task stays in progress.
fn a_recovery_target_the_lifecycle_does_not_allow_is_refused_with_its_rule() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let claim = claim_id(&pair, &leaf);

    let done = recover(&pair, &claim, "running", TaskStatus::Done, "done")
        .expect_err("done does not follow in-progress")
        .to_string();
    assert!(
        done.contains("'done' is reachable only from 'review'"),
        "{done}"
    );
    let in_progress = recover(&pair, &claim, "running", TaskStatus::InProgress, "stay")
        .expect_err("recovery fences the attempt")
        .to_string();
    assert!(
        in_progress.contains("cannot leave the task 'in-progress'"),
        "{in_progress}"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
    assert_eq!(pair.owner_claim(&claim)["claim"]["phase"], "running");
}

/// A handed-off claim holds its task in review, where `done` is a lifecycle
/// edge. Its accepted handoff carried the execution summary an ordinary
/// completion needs, so the picker offers `done` and one recovery fences the
/// attempt and completes the task (as for a pull request merged by hand).
/// The refusal side of the completion gate is in `post_deploy`.
fn a_handed_off_claim_recovers_to_done_with_its_completion_evidence() {
    let pair = Pair::new(1);
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_hands_off(&leaf);
    let settled = pair.pass(&drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.owner_status(&task), "review");
    let claim = claim_id(&pair, &leaf);
    assert_eq!(pair.owner_claim(&claim)["claim"]["phase"], "handed_off");
    assert_eq!(
        console_targets(&pair, &claim),
        [
            "blocked", "backlog", "review", "done", "archived", "rejected"
        ],
    );

    let completed = recover(&pair, &claim, "handed_off", TaskStatus::Done, "done").unwrap();

    assert_eq!(completed["task_status"], "done", "{completed}");
    assert_eq!(pair.owner_status(&task), "done");
    assert_eq!(pair.owner_claim(&claim)["claim"]["phase"], "revoked");
}

crate::scenario_table!(
    claim_recovery_target_scenarios,
    a_running_claim_recovers_to_a_lifecycle_valid_status_beyond_blocked_or_backlog,
    a_recovery_target_the_lifecycle_does_not_allow_is_refused_with_its_rule,
    a_handed_off_claim_recovers_to_done_with_its_completion_evidence,
);
