#![allow(missing_docs)]

//! The shipped PR pipeline routes on `candidate_resume`'s decision
//! [ORB-13985].
//!
//! The action's own decisions (squash-applying the candidate, validating it,
//! the spec and discard checks) are covered over a real repository in
//! `v2_worktree_lifecycle`; here the shipped job graph runs with scripted
//! stand-ins, so what is pinned is the routing: a validated candidate skips
//! implementation and delivers, a repair hands the implementer its trigger.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^candidate_resume::/)'`.

use serde_json::{Value, json};

use crate::review_fixes::{Revalidation, ScriptedHost, Settlement, position, run_shipped_pipeline};

/// A resumed candidate that validated runs no implementation step; the
/// pipeline commits, validates, reviews and delivers what is in the checkout.
#[test]
fn a_validated_candidate_delivers_without_an_implementation_step() {
    let host = ScriptedHost::new(Settlement::Accept, Revalidation::Passes).resuming(resumed(
        "resumed_validated",
        false,
        Value::Null,
    ));
    let result = run_shipped_pipeline(&host);

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let actions = host.actions();
    assert!(
        host.inputs("agent_implement").is_empty(),
        "no implementation step: {actions:?}"
    );
    let resume = position(&actions, "candidate_resume");
    assert_eq!(
        actions[resume + 1],
        "git_commit",
        "the resumed candidate goes straight to delivery: {actions:?}"
    );
    for step in ["candidate_validate", "git_push", "pr_open", "pr_complete"] {
        position(&actions, step);
    }
    assert!(host.inputs("pr_failure_handoff").is_empty());
}

/// A candidate that needs repair runs the implementation step, which gets
/// the repair trigger and output; a fresh one gets none.
#[test]
fn a_repair_hands_the_implementer_the_candidate_trigger() {
    let repair = json!({
        "trigger": "validation",
        "command": "make test",
        "exit_code": 2,
        "timed_out": false,
        "output": "test widget::renders ... FAILED",
    });
    let host = ScriptedHost::new(Settlement::Accept, Revalidation::Passes).resuming(resumed(
        "resumed_repaired",
        true,
        repair.clone(),
    ));
    let result = run_shipped_pipeline(&host);

    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let implement = host.inputs("agent_implement");
    assert_eq!(implement.len(), 1);
    assert_eq!(implement[0]["resume_candidate"], repair);

    let fresh = ScriptedHost::new(Settlement::Accept, Revalidation::Passes);
    run_shipped_pipeline(&fresh).expect("a fresh run delivers");
    let implement = fresh.inputs("agent_implement");
    assert_eq!(implement.len(), 1);
    assert_eq!(implement[0]["resume_candidate"], Value::Null);
}

fn resumed(outcome: &str, implement: bool, repair: Value) -> Value {
    json!({
        "phase": "candidate_resume",
        "outcome": outcome,
        "implement": implement,
        "reason": null,
        "repair": repair,
        "source_run_id": "failed-run",
        "source_branch": "orbit/T-1-candidate",
        "source_sha": "candidate",
        "base_sha": "base-sha",
    })
}
