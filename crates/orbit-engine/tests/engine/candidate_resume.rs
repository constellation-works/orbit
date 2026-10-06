#![allow(missing_docs)]

//! The shipped PR pipeline routes on `candidate_resume`'s decision
//! [ORB-13985].
//!
//! The action's own decisions (squash-applying the candidate, validating it,
//! the spec and discard checks) are covered over a real repository in
//! `v2_worktree_lifecycle`; here the shipped job graph runs with scripted
//! stand-ins, so what is pinned is the routing: a validated candidate skips
//! implementation and delivers, a repair hands the implementer its trigger,
//! and both claimed leaves hand the candidate their claim carries to it.
//!
//! Runs under `cargo nextest run -p orbit-engine --test engine -E 'test(/^candidate_resume::/)'`.

use serde_json::{Value, json};

use crate::review_fixes::{
    Revalidation, ScriptedHost, Settlement, position, run_shipped_job, run_shipped_pipeline,
};

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

/// [ORB-14257] A claimed PR leaf hands the candidate its claim carries to
/// `candidate_resume` in claimed mode, and the repair it decides to the
/// claimed implementer; a claim without one hands it nothing.
#[test]
fn a_claimed_leaf_hands_its_kept_candidate_to_resume_and_the_repair_to_the_implementer() {
    let kept = json!({
        "branch": "orbit/T-1-kept",
        "head_sha": "kept-sha",
        "source_run_id": "jrun-earlier-claim",
        "failed_step_id": "sync_base",
    });
    let repair = json!({
        "trigger": "continuation",
        "failed_step_id": "sync_base",
        "output": "continue the kept candidate",
    });
    let input = |candidate: Value| {
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "remote",
            "resume_candidate": candidate,
        })
    };
    let host = ScriptedHost::new(Settlement::Accept, Revalidation::Passes).resuming(resumed(
        "resumed_repaired",
        true,
        repair.clone(),
    ));
    let result = run_shipped_job(&host, "task_claimed_pr_pipeline", input(kept.clone()));
    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let resume = host.inputs("candidate_resume");
    assert_eq!(resume.len(), 1, "{:?}", host.actions());
    assert_eq!(resume[0]["claimed"], true);
    assert_eq!(resume[0]["candidate"], kept);
    let implement = host.inputs("agent_implement");
    assert_eq!(implement.len(), 1);
    assert_eq!(implement[0]["claimed"], true);
    assert_eq!(implement[0]["resume_candidate"], repair);

    let fresh = ScriptedHost::new(Settlement::Accept, Revalidation::Passes);
    run_shipped_job(&fresh, "task_claimed_pr_pipeline", input(Value::Null))
        .expect("a fresh claimed leaf delivers");
    assert_eq!(
        fresh.inputs("candidate_resume")[0]["candidate"],
        Value::Null
    );
    assert_eq!(
        fresh.inputs("agent_implement")[0]["resume_candidate"],
        Value::Null
    );
}

/// [ORB-14338] A claimed owner-local leaf runs `candidate_resume` too: the
/// candidate its claim carries reaches the action in claimed mode, and the
/// repair it decides reaches the claimed implementer.
#[test]
fn a_claimed_local_leaf_resumes_its_kept_candidate() {
    let kept = json!({
        "branch": "orbit/T-1-kept",
        "head_sha": "kept-sha",
        "source_run_id": "jrun-earlier-claim",
        "failed_step_id": "validate",
    });
    let repair = json!({
        "trigger": "continuation",
        "failed_step_id": "validate",
        "output": "continue the kept candidate",
    });
    let input = |candidate: Value| {
        json!({
            "task_ids": ["T-1"],
            "base_branch": "main",
            "base_sync": "local",
            "resume_candidate": candidate,
        })
    };
    let host = ScriptedHost::new(Settlement::Accept, Revalidation::Passes).resuming(resumed(
        "resumed_repaired",
        true,
        repair.clone(),
    ));
    let result = run_shipped_job(&host, "task_claimed_local_pipeline", input(kept.clone()));
    assert!(
        matches!(&result, Ok(outcome) if outcome.success),
        "{result:?}"
    );
    let actions = host.actions();
    let resume = host.inputs("candidate_resume");
    assert_eq!(resume.len(), 1, "{actions:?}");
    assert_eq!(resume[0]["claimed"], true);
    assert_eq!(resume[0]["candidate"], kept);
    assert_eq!(
        resume[0]["workspace_path"],
        host.inputs("agent_implement")[0]["workspace_path"]
    );
    assert!(
        position(&actions, "candidate_resume") < position(&actions, "agent_implement"),
        "the candidate is applied before the implementer runs: {actions:?}"
    );
    let implement = host.inputs("agent_implement");
    assert_eq!(implement.len(), 1);
    assert_eq!(implement[0]["claimed"], true);
    assert_eq!(implement[0]["resume_candidate"], repair);
    position(&actions, "claim_handoff");

    let fresh = ScriptedHost::new(Settlement::Accept, Revalidation::Passes);
    run_shipped_job(&fresh, "task_claimed_local_pipeline", input(Value::Null))
        .expect("a fresh claimed-local leaf hands off");
    assert_eq!(
        fresh.inputs("candidate_resume")[0]["candidate"],
        Value::Null
    );
    assert_eq!(
        fresh.inputs("agent_implement")[0]["resume_candidate"],
        Value::Null
    );
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
