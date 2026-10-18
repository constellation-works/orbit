//! [ORB-16727] A claim that exhausted its review budget is recovered by the
//! owner's `task requeue`. The task's next claim executes through the
//! production claimed worker and hands the kept candidate off.
//!
//! The owner drains its own backlog in local ship mode, so every step is
//! real Git against the owner's real claim journal. The first claim commits a
//! real candidate in its worktree, and its review budget is spent. Its
//! escalation names the requeue. After the operator runs it, the next claim's
//! queued leaf runs through `execute_pipeline_run_worker`: the worker's
//! binding is checked against the durable admission, and then the shipped
//! `task_claimed_local_pipeline` runs every step. `candidate_resume` applies
//! the kept commit, a scripted provider stands in for the implementer, and
//! `commit`, `validate` and `handoff` deliver the candidate. The owner's
//! required command passes only when the kept change is applied. Workers
//! without a binding, or bound to the failed claim or to another run, are
//! refused before any step runs.

use super::*;
use orbit_types::workflow::{ExecutorSandboxKind, ReviewBudget, ReviewLedger};

/// The owner's required check, which passes only on a checkout carrying the
/// kept candidate. Its output names the line, so the log shows it ran there.
const KEPT_CHECK: &str = "grep -n kept src/f0.rs";
const KEPT: &str = "fn work() {\n    kept();\n}\n";
const REQUEUE_REASON: &str = "requeue after the claimed leaf exhausted its review budget";
/// The crew the scripted implementer answers for.
const IMPLEMENTER_CREW: &str = "default_crew = \"implementer\"\n\n\
                                [crews.implementer]\nprovider = \"codex\"\nmodel = \"fixture\"\n";
const SUMMARY: &str = "Outcome: success\nContinued the candidate the failed claim kept.";

fn a_requeued_claimed_review_continues_its_kept_candidate_through_the_bound_worker() {
    let owner = OwnerLocal::requiring(KEPT_CHECK, IMPLEMENTER_CREW);
    let task = owner.pair.tasks[0].clone();
    owner
        .pair
        .wire
        .owner
        .update_task_as_human(
            &task,
            orbit_core::application::task::TaskUpdateParams {
                crew: Some(Some("implementer".into())),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .expect("the task runs on the scripted crew");
    let base = git(&owner.pair.owner_repo, &["rev-parse", "main"])
        .trim()
        .to_string();

    // The first claim commits a real candidate on its worktree branch, then
    // spends its claim's whole review budget and escalates.
    let (first, worker) = owner.pair.running_leaf_with_worker(&owner.drain, 1);
    assert_eq!(owner.pair.claimed_task(&first), task);
    let first_claim = owner.claim_of(&first);
    let worktree = engine_action(
        &owner.bound(&first),
        "worktree_setup",
        &json!({
            "job_run_id": first, "run_id": first, "task_ids": [task],
            "base": "main", "base_sync": "local", "landing_mode": "local",
        }),
    );
    let checkout = PathBuf::from(worktree["workspace_path"].as_str().unwrap());
    std::fs::write(checkout.join(FILE), KEPT).unwrap();
    git(
        &checkout,
        &["commit", "-q", "-am", "The candidate the claim keeps"],
    );
    let kept = git(&checkout, &["rev-parse", "HEAD"]).trim().to_string();
    let workspace = owner.pair.wire.owner.workspace_id().unwrap();
    let mut ledger = ReviewLedger::new(
        &format!("{workspace}/{task}/main/{first_claim}"),
        vec![task.clone()],
        ReviewBudget { minutes: 30 },
        Utc::now(),
    );
    ledger.consumed_seconds = 1_800;
    owner
        .pair
        .wire
        .owner
        .review_store()
        .unwrap()
        .review_import_claimed_ledger(&workspace, &ledger)
        .unwrap();
    let mut state = owner.pair.follower.read_run_state(&first).unwrap().unwrap();
    state.pipeline["worktree"] = worktree.clone();
    state.pipeline["commit"] = json!({"commit_sha": kept});
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        key: FinalRecoveryKey {
            run_id: first.clone(),
            attempt: 1,
        },
        failed_step_id: "validate".into(),
        task_id: task.clone(),
        observed: None,
        repair_commit: None,
        resume_context: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: Some(FinalRecoveryDecision::Escalate {
            diagnosis: "review budget exhausted".into(),
            human_action: "Authorize a review reset".into(),
        }),
        outcome: Some("settled: recorded for the claim settlement".into()),
    });
    owner.pair.follower.write_run_state(&first, &state).unwrap();
    owner.pair.leaf_fails_with(
        &first,
        "review_budget_exhausted: review_minutes_exhausted for this claim's lineage",
    );
    let settled = owner.pair.pass(&owner.drain);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(owner.claim_phase(&first_claim), "failed");
    assert_eq!(owner.pair.owner_status(&task), "blocked");
    let comments = comments_of(&owner.pair.owner_task(&task));
    assert!(
        comments.contains(&format!("task requeue {task} --reason "))
            && comments.contains("continues the candidate this claim kept")
            && !comments.contains(&format!("job resume {first}")),
        "the escalation names the owner's requeue, never a resume: {comments}"
    );

    // The operator runs the requeue the escalation names, once the failed
    // leaf's worker has exited.
    drop(worker);
    owner
        .pair
        .wire
        .owner
        .requeue_blocked_task(&task, REQUEUE_REASON, false)
        .expect("the owner requeues the blocked task");
    assert_eq!(owner.pair.owner_status(&task), "backlog");

    // The next claim's leaf is queued and bound as the launcher leaves it,
    // before its worker starts.
    let next = owner.pair.queued_leaf(&owner.drain, 1);
    assert_eq!(
        owner.pair.claimed_task(&next),
        task,
        "the task is claimed again"
    );
    let next_claim = owner.claim_of(&next);
    assert_ne!(
        next_claim, first_claim,
        "the continuation is a new admission"
    );
    owner.pair.advance(&next, LocalPullMutation::Bound);
    owner.pair.advance(&next, LocalPullMutation::LaunchIntent);
    owner.pair.advance(&next, LocalPullMutation::Launched);
    let offered = super::super::candidate_carry::claim_candidate(&owner.pair, &next)
        .expect("the requeue keeps the candidate");
    assert_eq!(offered.head_sha, kept);

    // The shipped job and activities, as `orbit init` deploys them. No worker
    // without this claim's binding runs the leaf.
    orbit_core::bootstrap::init::init_workspace_at_root(
        &owner.pair.wire.owner.global_root(),
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            refresh_defaults: true,
            ..Default::default()
        },
    )
    .expect("deploy the shipped jobs and activities");
    let binding = owner.bound(&next).worker_invocation().cloned().unwrap();
    let refusals = [
        ("an unbound worker", owner.pair.follower.clone()),
        (
            "a worker bound to the failed claim",
            owner.bound_as(orbit_types::tool::WorkerInvocation {
                claim_id: first_claim.clone(),
                ..binding.clone()
            }),
        ),
        (
            "a worker bound to another run",
            owner.bound_as(orbit_types::tool::WorkerInvocation {
                bound_run_id: first.clone(),
                ..binding.clone()
            }),
        ),
    ];
    for (case, worker) in refusals {
        let refused = worker
            .execute_pipeline_run_worker(&next)
            .expect_err(&format!("{case} must be refused"));
        assert!(
            matches!(refused, OrbitError::PolicyDenied(_)),
            "{case}: {refused}"
        );
        assert_eq!(owner.pair.run_state(&next), JobRunState::Pending, "{case}");
        assert!(
            owner
                .pair
                .follower
                .read_run_state(&next)
                .unwrap()
                .is_none_or(|state| state
                    .step_states
                    .values()
                    .all(|step| *step != JobRunState::Success)),
            "{case} ran a step"
        );
    }

    // The implementer is the one scripted step: it leaves the applied
    // candidate as it found it.
    implementer_succeeds(&owner.pair);
    owner
        .bound(&next)
        .execute_pipeline_run_worker(&next)
        .expect("the claim's bound worker runs its leaf");
    assert_eq!(owner.pair.run_state(&next), JobRunState::Success);
    let state = owner.pair.follower.read_run_state(&next).unwrap().unwrap();
    let resumed = &state.pipeline["resume_candidate"];
    assert_eq!(resumed["source_sha"], kept.as_str(), "{resumed}");
    assert_eq!(resumed["source_run_id"], first.as_str(), "{resumed}");
    for step in ["commit", "validate", "handoff"] {
        assert!(
            state
                .pipeline
                .get(step)
                .is_some_and(|output| !output.is_null()),
            "the worker ran `{step}`: {:#}",
            state.pipeline
        );
    }

    let delivered = owner.pair.pass(&owner.drain);
    assert!(delivered["error"].is_null(), "{delivered}");
    assert_eq!(owner.claim_phase(&next_claim), "handed_off");
    assert_eq!(
        owner.claim_phase(&first_claim),
        "failed",
        "the failed claim never regains authority"
    );
    assert_eq!(owner.pair.owner_status(&task), "review");
    let accepted = owner.accepted(&next_claim);
    assert_eq!(accepted.handoff.run_id, next);
    assert_eq!(accepted.required_commands, [KEPT_CHECK]);
    let candidate = accepted.handoff.candidate.candidate.commit.clone();
    let repo = &owner.pair.owner_repo;
    assert_eq!(
        git(repo, &["show", &format!("{candidate}:{FILE}")]),
        KEPT,
        "the handed-off candidate carries the kept change"
    );
    assert_eq!(
        git(repo, &["rev-parse", &format!("{candidate}^")]).trim(),
        base,
        "the candidate is built on the base"
    );
    let logs = accepted
        .handoff
        .validation
        .iter()
        .map(|log| {
            let artifact = owner
                .pair
                .wire
                .owner
                .get_task_artifact(&task, &log.path)
                .unwrap()
                .expect("the owner holds the validation log");
            serde_json::from_slice::<orbit_types::workflow::handoff::HandoffValidationLog>(
                &artifact.content,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(
        logs.iter().any(|log| log.command == KEPT_CHECK
            && log.exit_code == 0
            && log.tested_head == candidate
            && log.output.contains("kept();")),
        "the required command ran on the handed-off candidate: {logs:#?}"
    );
}

/// The implementer crew's provider answers with a claimed success that
/// leaves the checkout as `candidate_resume` left it.
fn implementer_succeeds(pair: &Pair) {
    let envelope = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {"execution_summary": SUMMARY},
        "error": null,
    });
    let bin = pair._root.path().join("implementer");
    std::fs::create_dir_all(&bin).unwrap();
    let runtime = &pair.wire.owner;
    let providers = ["codex"];
    for provider in providers {
        // The executor reads its provider from the command's file name.
        let script = bin.join(provider);
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{envelope}'\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut executor = runtime.get_executor_def(provider).unwrap().unwrap();
        executor.command = Some(script.to_string_lossy().into_owned());
        executor.sandbox = Some(ExecutorSandboxKind::Off);
        runtime.upsert_executor_def(&executor).unwrap();
    }
}

crate::scenario_table!(
    requeued_claim_scenarios,
    a_requeued_claimed_review_continues_its_kept_candidate_through_the_bound_worker
);
