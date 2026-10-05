//! The final-recovery applier: every non-`resume` decision, and the checks
//! that keep an agent's proposal from overriding the base branch, the requeue
//! bound, or a human.

use std::path::Path;

use orbit_common::fs::git::run_git;
use orbit_engine::activity_job::load_activity_asset;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{ActivityV2Spec, FINAL_RECOVERY_CREWS_KEY, FinalRecoveryDecision};
use serde_json::{Value, json};

use super::{enter_isolated_child, test_runtime};
use crate::OrbitRuntime;
use crate::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    FinalRecoveryTaskRevision, TaskAddParams, TaskUpdateParams,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

const RUN_ID: &str = "jrun-20261004-1200-t1";

/// A repository whose `main` holds one commit and whose `side` branch holds
/// one more that never reached `main`. Returns `(on_main, off_main)`.
fn repository(path: &Path) -> (String, String) {
    let git = |args: &[&str]| {
        let output = run_git(path, args).expect("run git");
        assert!(output.success, "git {args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    };
    let commit = |message: &str| {
        git(&[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            message,
        ]);
        git(&["rev-parse", "HEAD"])
    };
    git(&["init", "-q", "-b", "main"]);
    let on_main = commit("delivered on main");
    git(&["checkout", "-q", "-b", "side"]);
    let off_main = commit("never merged");
    git(&["checkout", "-q", "main"]);
    (on_main, off_main)
}

/// An in-progress task, as a failed run leaves it.
fn failed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise the final-recovery applier.".to_string(),
            acceptance_criteria: vec!["The decision is applied exactly once.".to_string()],
            plan: "1) implement 2) validate".to_string(),
            ..Default::default()
        })
        .expect("add task");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .expect("start task")
}

fn request(task: &Task, repo: &Path, completion: FinalRecoveryCompletion) -> FinalRecoveryRequest {
    FinalRecoveryRequest {
        task_id: task.id.clone(),
        run_id: RUN_ID.to_string(),
        observed: FinalRecoveryTaskRevision::of(task),
        repo_root: repo.to_path_buf(),
        base_ref: "main".to_string(),
        completion,
        requeue_bound: FinalRecoveryRequeueBound::default(),
    }
}

fn apply(
    runtime: &OrbitRuntime,
    request: &FinalRecoveryRequest,
    output: Value,
) -> FinalRecoveryOutcome {
    runtime
        .apply_final_recovery(request, Some(&output))
        .expect("apply decision")
}

/// The one comment the applier wrote for this run's decision.
fn decision_comment(runtime: &OrbitRuntime, task_id: &str) -> String {
    let comments = runtime
        .get_task_comments(task_id)
        .expect("read comments")
        .into_iter()
        .filter(|comment| comment.message.contains(&format!("run_id={RUN_ID}")))
        .collect::<Vec<_>>();
    assert_eq!(comments.len(), 1, "exactly one decision comment per apply");
    comments[0].message.clone()
}

fn status(runtime: &OrbitRuntime, task_id: &str) -> TaskStatus {
    runtime.get_task(task_id).expect("read task").status
}

#[test]
fn complete_no_diff_completes_only_on_a_commit_reachable_from_base() {
    if !enter_isolated_child(
        module_path!(),
        "complete_no_diff_completes_only_on_a_commit_reachable_from_base",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let repo = root.path().join("repo");
    let (on_main, off_main) = repository(&repo);

    let done = failed_task(&runtime, "Already landed, done authority");
    let outcome = apply(
        &runtime,
        &request(&done, &repo, FinalRecoveryCompletion::Done),
        json!({"decision": "complete_no_diff", "evidence_commit": &on_main[..12], "rationale": "landed in main"}),
    );
    assert_eq!(
        outcome,
        FinalRecoveryOutcome::Completed {
            status: TaskStatus::Done,
            evidence_commit: on_main.clone(),
        }
    );
    let completed = runtime.get_task(&done.id).expect("read task");
    assert_eq!(completed.status, TaskStatus::Done);
    assert!(
        !completed.execution_summary.trim().is_empty(),
        "done needs a summary, so the applier supplies one naming the covering commit"
    );
    assert!(decision_comment(&runtime, &done.id).contains("decision=complete_no_diff"));

    let review = failed_task(&runtime, "Already landed, review authority");
    let outcome = apply(
        &runtime,
        &request(&review, &repo, FinalRecoveryCompletion::Review),
        json!({"decision": "complete_no_diff", "evidence_commit": on_main, "rationale": "landed"}),
    );
    assert!(matches!(
        outcome,
        FinalRecoveryOutcome::Completed {
            status: TaskStatus::Review,
            ..
        }
    ));
    assert_eq!(status(&runtime, &review.id), TaskStatus::Review);

    for (title, commit) in [
        ("Unmerged covering commit", off_main),
        ("Unknown covering commit", "abcdef0123456789".to_string()),
    ] {
        let task = failed_task(&runtime, title);
        let outcome = apply(
            &runtime,
            &request(&task, &repo, FinalRecoveryCompletion::Done),
            json!({"decision": "complete_no_diff", "evidence_commit": commit, "rationale": "trust me"}),
        );
        let FinalRecoveryOutcome::Escalated {
            reason: Some(reason),
        } = outcome
        else {
            panic!("an unverified covering commit must escalate, got {outcome:?}");
        };
        assert!(reason.contains("complete_no_diff refused"), "{reason}");
        assert_eq!(
            status(&runtime, &task.id),
            TaskStatus::Blocked,
            "{title}: never completed on an unverified commit"
        );
    }
}

#[test]
fn reject_archive_and_escalate_record_the_decision_with_the_run_id() {
    if !enter_isolated_child(
        module_path!(),
        "reject_archive_and_escalate_record_the_decision_with_the_run_id",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let repo = root.path();
    for (output, expected_outcome, expected_status) in [
        (
            json!({"decision": "reject", "reason": "contradicts AGENTS.md", "evidence": "rule 3"}),
            FinalRecoveryOutcome::Rejected,
            TaskStatus::Rejected,
        ),
        (
            json!({"decision": "archive", "reason": "superseded by a landed task"}),
            FinalRecoveryOutcome::Archived,
            TaskStatus::Archived,
        ),
        (
            json!({"decision": "escalate", "diagnosis": "provider 403", "human_action": "grant repo access"}),
            FinalRecoveryOutcome::Escalated { reason: None },
            TaskStatus::Blocked,
        ),
    ] {
        let kind = output["decision"].as_str().expect("decision").to_string();
        let task = failed_task(&runtime, &kind);
        let outcome = apply(
            &runtime,
            &request(&task, repo, FinalRecoveryCompletion::Done),
            output,
        );
        assert_eq!(outcome, expected_outcome, "{kind}");
        assert_eq!(status(&runtime, &task.id), expected_status, "{kind}");
        assert!(
            decision_comment(&runtime, &task.id).contains(&format!("decision={kind}")),
            "{kind}"
        );
    }
}

#[test]
fn requeue_returns_to_backlog_until_the_window_bound_then_escalates() {
    if !enter_isolated_child(
        module_path!(),
        "requeue_returns_to_backlog_until_the_window_bound_then_escalates",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let task = failed_task(&runtime, "Flaky environment");
    let requeue = json!({"decision": "requeue", "reason": "bwrap now resolves"});

    // Each failure is its own run; the applier applies a run's decision once.
    let run = |attempt: usize, current: &Task| FinalRecoveryRequest {
        run_id: format!("{RUN_ID}-{attempt}"),
        ..request(current, root.path(), FinalRecoveryCompletion::Done)
    };
    let bound = FinalRecoveryRequeueBound::default().max_requeues;
    for attempt in 1..=bound {
        let current = runtime.get_task(&task.id).expect("read task");
        let outcome = apply(&runtime, &run(attempt, &current), requeue.clone());
        assert_eq!(outcome, FinalRecoveryOutcome::Requeued, "requeue {attempt}");
        assert_eq!(status(&runtime, &task.id), TaskStatus::Backlog);
        // The next run takes the task again and fails again.
        runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .expect("restart task");
    }

    let current = runtime.get_task(&task.id).expect("read task");
    let outcome = apply(&runtime, &run(bound + 1, &current), requeue);
    let FinalRecoveryOutcome::Escalated {
        reason: Some(reason),
    } = outcome
    else {
        panic!("a requeue past the bound must escalate, got {outcome:?}");
    };
    assert!(reason.contains("requeue refused"), "{reason}");
    assert_eq!(status(&runtime, &task.id), TaskStatus::Blocked);
}

#[test]
fn a_task_changed_after_the_failure_or_already_settled_is_refused() {
    if !enter_isolated_child(
        module_path!(),
        "a_task_changed_after_the_failure_or_already_settled_is_refused",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let task = failed_task(&runtime, "Operator stepped in");
    let observed = request(&task, root.path(), FinalRecoveryCompletion::Done);
    runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("Withdrawn: do not retry this.".to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("operator comments after the failure");

    let outcome = apply(
        &runtime,
        &observed,
        json!({"decision": "requeue", "reason": "looks environmental"}),
    );
    let FinalRecoveryOutcome::Refused { reason } = outcome else {
        panic!("a human change must win, got {outcome:?}");
    };
    assert!(reason.contains("changed after the failure"), "{reason}");
    assert_eq!(status(&runtime, &task.id), TaskStatus::InProgress);
    assert!(decision_comment(&runtime, &task.id).contains("outcome=refused"));

    let settled = failed_task(&runtime, "Already archived");
    runtime
        .update_task(
            &settled.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .expect("archive task");
    let archived = runtime.get_task(&settled.id).expect("read task");
    let outcome = apply(
        &runtime,
        &request(&archived, root.path(), FinalRecoveryCompletion::Done),
        json!({"decision": "requeue", "reason": "retry"}),
    );
    assert!(
        matches!(outcome, FinalRecoveryOutcome::Refused { .. }),
        "settled work is never reopened, got {outcome:?}"
    );
    assert_eq!(status(&runtime, &settled.id), TaskStatus::Archived);
}

#[test]
fn malformed_output_escalates_and_resume_writes_nothing() {
    if !enter_isolated_child(
        module_path!(),
        "malformed_output_escalates_and_resume_writes_nothing",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    for (label, output) in [
        ("missing field", Some(json!({"decision": "archive"}))),
        (
            "empty field",
            Some(json!({"decision": "archive", "reason": "  "})),
        ),
        (
            "unknown decision",
            Some(json!({"decision": "retry", "reason": "x"})),
        ),
        (
            "unknown field",
            Some(json!({"decision": "archive", "reason": "x", "force": true})),
        ),
        (
            "non-hex commit",
            Some(
                json!({"decision": "complete_no_diff", "evidence_commit": "--all", "rationale": "x"}),
            ),
        ),
        ("no result", None),
    ] {
        let task = failed_task(&runtime, label);
        let outcome = runtime
            .apply_final_recovery(
                &request(&task, root.path(), FinalRecoveryCompletion::Done),
                output.as_ref(),
            )
            .expect("apply malformed output");
        assert_eq!(
            outcome,
            FinalRecoveryOutcome::Escalated { reason: None },
            "{label}"
        );
        assert_eq!(status(&runtime, &task.id), TaskStatus::Blocked, "{label}");
        assert!(
            decision_comment(&runtime, &task.id).contains("malformed decision"),
            "{label}"
        );
    }

    let task = failed_task(&runtime, "Repaired worktree");
    let outcome = apply(
        &runtime,
        &request(&task, root.path(), FinalRecoveryCompletion::Done),
        json!({"decision": "resume", "step_id": "commit", "rationale": "conflict resolved"}),
    );
    assert_eq!(
        outcome,
        FinalRecoveryOutcome::Resume {
            step_id: "commit".to_string()
        }
    );
    assert_eq!(
        FinalRecoveryTaskRevision::of(&runtime.get_task(&task.id).expect("read task")),
        FinalRecoveryTaskRevision::of(&task),
        "resume is the engine's to apply; the applier writes nothing"
    );
}

/// The shipped activity is what produces the decisions above, so its declared
/// output must offer exactly the decisions the applier parses, route its crew
/// through the final-recovery pool, and withhold task writes and resume.
#[test]
fn shipped_final_recovery_activity_offers_exactly_the_typed_decisions() {
    let (_, yaml) = DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "final_recovery")
        .expect("final_recovery activity is shipped");
    let asset = load_activity_asset(yaml).expect("parse final_recovery activity");
    assert_eq!(
        asset.spec.input_schema_json["properties"]["crew_config_key"]["const"],
        FINAL_RECOVERY_CREWS_KEY
    );
    let offered = asset.spec.output_schema_json["oneOf"]
        .as_array()
        .expect("one schema per decision")
        .iter()
        .map(|variant| {
            variant["properties"]["decision"]["const"]
                .as_str()
                .expect("decision tag")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(offered, FinalRecoveryDecision::KINDS);

    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("final_recovery must be an agent_loop activity");
    };
    assert!(spec.require_response_envelope, "the decision is consumed");
    let denied = spec.tool_disallow_list.unwrap_or_default();
    for tool in [
        "orbit.task.update",
        "orbit.workflow.run.resume",
        "orbit.pipeline.invoke",
    ] {
        assert!(
            denied.iter().any(|entry| entry == tool),
            "final recovery decides; the applier and engine act, so `{tool}` stays denied"
        );
    }
}
