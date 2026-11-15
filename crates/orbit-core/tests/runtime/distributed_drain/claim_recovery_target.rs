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
        ("operator-recover", key),
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

/// A failed claim can outlive a requeue and a newer claim. The owner console
/// must withhold recovery from that historical attempt while preserving it
/// for the current attempt, including when that attempt also fails.
fn a_historical_failed_claim_has_no_recovery_targets_after_a_new_pull() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let first = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&first);
    let historical = claim_id(&pair, &first);
    pair.leaf_fails_with(&first, "candidate implementation failed");
    pair.pass(&drain);
    assert_eq!(pair.owner_status(&task), "blocked");
    assert_eq!(pair.owner_claim(&historical)["claim"]["phase"], "failed");
    let failed_targets = console_targets(&pair, &historical);
    assert!(
        !failed_targets.is_empty(),
        "the current failed claim offers recovery"
    );

    pair.wire
        .owner
        .requeue_blocked_task(&task, "retry the stopped candidate", false)
        .unwrap();
    let next = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&next), task);
    let current = claim_id(&pair, &next);
    assert_ne!(historical, current);
    assert!(
        console_targets(&pair, &historical).is_empty(),
        "a historical failed claim must never offer recovery against the newer claim's task"
    );
    assert!(!console_targets(&pair, &current).is_empty());
    let console = pair.wire.owner.distributed_claim_console().unwrap();
    for (id, expected_current) in [(&historical, false), (&current, true)] {
        let row = console["claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["claim_id"] == *id)
            .unwrap();
        assert_eq!(row["current"], expected_current, "{row:#}");
    }

    pair.leaf_fails_with(&next, "candidate implementation failed again");
    pair.pass(&drain);
    assert_eq!(console_targets(&pair, &current), failed_targets);
    assert!(console_targets(&pair, &historical).is_empty());
    let recovered = recover(
        &pair,
        &current,
        "failed",
        TaskStatus::Archived,
        "archive-failed",
    )
    .unwrap();
    assert_eq!(recovered["phase"], "revoked");
    assert_eq!(pair.owner_status(&task), "archived");
    assert_eq!(pair.owner_claim(&historical)["claim"]["phase"], "failed");
}

crate::scenario_table!(
    claim_recovery_target_scenarios,
    a_running_claim_recovers_to_a_lifecycle_valid_status_beyond_blocked_or_backlog,
    a_recovery_target_the_lifecycle_does_not_allow_is_refused_with_its_rule,
    a_handed_off_claim_recovers_to_done_with_its_completion_evidence,
    a_historical_failed_claim_has_no_recovery_targets_after_a_new_pull,
);

/// The same recovery through the `orbit.task.recover_claim` tool, which is the
/// CLI, MCP and remote-host path for the operator console.
fn recover_through_tool(
    pair: &Pair,
    claim_id: &str,
    reason: &str,
    request_key: &str,
) -> Result<Value, OrbitError> {
    let input = json!({
        "claim_id": claim_id,
        "expected_phase": "running",
        "status": "blocked",
        "reason": reason,
        "request_key": request_key,
    });
    pair.wire.owner.run_tool_with_context(
        "orbit.task.recover_claim",
        input,
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

/// Recovery bounds its replay key and reason in core before any claim changes:
/// the key needs 1..=128 bytes of ASCII letters, digits, `-` or `_` after
/// trimming, and the reason 1..=2000 bytes. Surrounding space is trimmed, so an
/// empty or whitespace-only key or reason is refused too.
#[test]
fn an_operator_recovery_key_and_reason_are_bounded_in_core_before_any_claim_changes() {
    let pair = Pair::new(1);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let claim = claim_id(&pair, &leaf);

    for key in ["", "   ", "has space", &"k".repeat(129)] {
        let error = recover(&pair, &claim, "running", TaskStatus::Blocked, key).unwrap_err();
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{key}: {error}"
        );
        let error = recover_through_tool(&pair, &claim, "reason", key).unwrap_err();
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{key}: {error}"
        );
    }
    for reason in ["", " \t\n ", &"r".repeat(2001)] {
        let error = pair
            .wire
            .owner
            .recover_claim_as_operator(
                &claim,
                "running",
                TaskStatus::Blocked,
                "operator",
                reason,
                ("operator-recover", "reason-bound"),
            )
            .unwrap_err();
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{} byte reason: {error}",
            reason.len()
        );
        let error = recover_through_tool(&pair, &claim, reason, "reason-bound").unwrap_err();
        assert!(
            matches!(error, OrbitError::InvalidInput(_)),
            "{} byte reason through the tool: {error}",
            reason.len()
        );
    }
    assert_eq!(pair.owner_claim(&claim)["claim"]["phase"], "running");

    // The boundary values pass, and padding around the key and expected phase
    // is trimmed in core, as the tool already did.
    let recovered = pair
        .wire
        .owner
        .recover_claim_as_operator(
            &claim,
            " running ",
            TaskStatus::Blocked,
            "operator",
            &"r".repeat(2000),
            ("operator-recover", &format!("  {}  ", "k".repeat(128))),
        )
        .unwrap();
    assert_eq!(recovered["task_status"], "blocked", "{recovered}");
}
