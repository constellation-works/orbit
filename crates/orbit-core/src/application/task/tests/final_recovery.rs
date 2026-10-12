//! The final-recovery applier: every non-`resume` decision, and the checks
//! that keep an agent's proposal from overriding the base branch, the requeue
//! bound, or a human.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_engine::activity_job::load_activity_asset;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use orbit_types::record::FrictionStatus;
use orbit_types::task::{ExternalRef, Task, TaskRelation, TaskRelationType, TaskStatus};
use orbit_types::workflow::{ActivityV2Spec, FINAL_RECOVERY_CREWS_KEY, FinalRecoveryDecision};
use serde_json::{Value, json};

use super::{enter_isolated_child, test_runtime};
use crate::OrbitRuntime;
use crate::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    TaskAddParams, TaskUpdateParams,
};
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;
use crate::runtime::task_pr_forge::{ForgePullRequest, TaskPrForge};

const RUN_ID: &str = "jrun-20261004-1200-t1";

#[derive(Default)]
struct RecoveryForge {
    open: Mutex<Option<ForgePullRequest>>,
    closed: Mutex<Vec<u64>>,
}

impl TaskPrForge for RecoveryForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(self.open.lock().unwrap().iter().cloned().collect())
    }

    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".to_string())
    }

    fn close_pull_request(&self, _: &Path, number: u64, _: &str) -> Result<(), OrbitError> {
        assert_eq!(self.open.lock().unwrap().take().unwrap().number, number);
        self.closed.lock().unwrap().push(number);
        Ok(())
    }
}

/// Fault injection is necessary to stop exactly after the durable decision,
/// before its first post-lock read, without relying on process timing.
#[test]
fn terminal_replay_finishes_side_effects_after_a_post_lock_read_failure() {
    if !enter_isolated_child(
        module_path!(),
        "terminal_replay_finishes_side_effects_after_a_post_lock_read_failure",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let repo = root.path().join("repo");
    let (commit, _) = repository(&repo);
    let forge = Arc::new(RecoveryForge::default());
    let runtime = runtime.with_task_pr_forge(forge.clone());
    let frictions = FrictionStore::open(
        runtime.sqlite_store().expect("store"),
        runtime.workspace_id().expect("workspace id"),
        runtime.data_root().join("frictions"),
    )
    .expect("friction store");

    for (output, expected_status) in [
        (
            json!({"decision": "complete_no_diff", "evidence_commit": commit, "rationale": "landed"}),
            TaskStatus::Done,
        ),
        (
            json!({"decision": "reject", "reason": "invalid", "evidence": "workspace rule"}),
            TaskStatus::Rejected,
        ),
        (
            json!({"decision": "archive", "reason": "superseded"}),
            TaskStatus::Archived,
        ),
    ] {
        let friction = frictions
            .add(FrictionAddParams {
                model: "codex".to_string(),
                title: None,
                body: "Final recovery resolves this target".to_string(),
                tags: vec!["tooling".to_string()],
                status: FrictionStatus::Open,
                during_task: None,
                created_at: Utc::now(),
            })
            .expect("add friction")
            .record
            .id;
        let task = failed_task(&runtime, "Recover terminal side effects");
        let task = runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    relations: Some(vec![TaskRelation {
                        relation_type: TaskRelationType::Resolves,
                        target: friction.clone(),
                    }]),
                    ..Default::default()
                },
            )
            .expect("record relation");
        runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    external_refs: vec![ExternalRef::github_pr("17").expect("PR reference")],
                    ..Default::default()
                },
            )
            .expect("record PR");
        let task = runtime.get_task(&task.id).expect("task with PR");
        *forge.open.lock().unwrap() = Some(ForgePullRequest {
            number: 17,
            head_branch: format!("orbit/{}-delivery", task.id),
            body: format!("Task {}", task.id),
            author: "orbit-bot".to_string(),
        });
        forge.closed.lock().unwrap().clear();
        let request = request(&runtime, &task, &repo, FinalRecoveryCompletion::Done);
        super::super::final_recovery::FAIL_SIDE_EFFECT_READ.with(|fail| fail.set(true));
        let error = runtime
            .apply_final_recovery(&request, Some(&output))
            .expect_err("post-lock read fails once");
        assert!(
            error
                .to_string()
                .contains("injected post-lock task read failure"),
            "{error}"
        );
        assert_eq!(status(&runtime, &task.id), expected_status);
        let comment = decision_comment(&runtime, &task.id);
        assert!(forge.closed.lock().unwrap().is_empty());
        assert_eq!(
            frictions.show(&friction).unwrap().unwrap().record.status,
            FrictionStatus::Open
        );

        // The durable outcome wins even if the caller changes the proposal.
        let outcome = apply(
            &runtime,
            &request,
            json!({"decision": "requeue", "reason": "ignored on replay"}),
        );
        assert_eq!(*forge.closed.lock().unwrap(), vec![17]);
        assert_eq!(
            frictions.show(&friction).unwrap().unwrap().record.status,
            if expected_status == TaskStatus::Done {
                FrictionStatus::Resolved
            } else {
                FrictionStatus::Open
            }
        );
        let history = runtime
            .get_task_history(&task.id)
            .expect("history after recovery");
        if expected_status == TaskStatus::Done {
            runtime
                .run_tool(
                    "orbit.friction.update",
                    json!({"id": friction, "status": "open"}),
                )
                .expect("human reopens target");
        }
        assert_eq!(apply(&runtime, &request, output), outcome);
        assert_eq!(decision_comment(&runtime, &task.id), comment);
        assert_eq!(
            runtime.get_task_history(&task.id).unwrap(),
            history,
            "successful effects are not repeated"
        );
        assert_eq!(*forge.closed.lock().unwrap(), vec![17]);
        assert_eq!(
            frictions.show(&friction).unwrap().unwrap().record.status,
            FrictionStatus::Open,
            "a successful resolve must not override a later human reopening"
        );
    }
}

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

/// A local checkout with a bare origin whose tracking ref can be left stale
/// while the remote advances.
fn bare_remote_repository(path: &Path) -> (std::path::PathBuf, String, String) {
    let remote = path.join("origin.git");
    let repo = path.join("checkout");
    std::fs::create_dir_all(path).expect("create fixture root");
    std::fs::create_dir_all(&repo).expect("create checkout");
    let git = |cwd: &Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run fixture git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(path, &["init", "-q", "--bare", "origin.git"]);
    git(&repo, &["init", "-q", "-b", "agent-main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "initial remote commit",
        ],
    );
    let remote_path = remote.to_string_lossy().into_owned();
    git(&repo, &["remote", "add", "origin", &remote_path]);
    git(&repo, &["push", "--quiet", "origin", "agent-main"]);
    git(
        &repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            "+refs/heads/agent-main:refs/remotes/origin/agent-main",
        ],
    );
    let fetched = git(&repo, &["rev-parse", "refs/remotes/origin/agent-main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "landed after the last fetch",
        ],
    );
    let evidence = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["push", "--quiet", "origin", "agent-main"]);
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/agent-main", &fetched],
    );
    (repo, fetched, evidence)
}

fn fixture_git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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

fn request(
    runtime: &OrbitRuntime,
    task: &Task,
    repo: &Path,
    completion: FinalRecoveryCompletion,
) -> FinalRecoveryRequest {
    FinalRecoveryRequest {
        task_id: task.id.clone(),
        run_id: RUN_ID.to_string(),
        observed: runtime.final_recovery_revision(task).expect("observe task"),
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
        &request(&runtime, &done, &repo, FinalRecoveryCompletion::Done),
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
        &request(&runtime, &review, &repo, FinalRecoveryCompletion::Review),
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
            &request(&runtime, &task, &repo, FinalRecoveryCompletion::Done),
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
fn complete_no_diff_refreshes_a_stale_remote_tracking_ref_once_before_refusing() {
    if !enter_isolated_child(
        module_path!(),
        "complete_no_diff_refreshes_a_stale_remote_tracking_ref_once_before_refusing",
    ) {
        return;
    }
    let (root, runtime) = test_runtime();
    let (repo, fetched, evidence) = bare_remote_repository(&root.path().join("remote-case"));
    assert_eq!(
        fixture_git(&repo, &["rev-parse", "refs/remotes/origin/agent-main"]),
        fetched,
        "the local tracking ref must retain the pre-push snapshot"
    );

    let task = failed_task(&runtime, "Refresh the stale remote tracking ref");
    let mut decision_request = request(&runtime, &task, &repo, FinalRecoveryCompletion::Done);
    decision_request.base_ref = "origin/agent-main".to_string();
    let outcome = apply(
        &runtime,
        &decision_request,
        json!({"decision": "complete_no_diff", "evidence_commit": &evidence[..12], "rationale": "landed remotely"}),
    );
    assert_eq!(
        outcome,
        FinalRecoveryOutcome::Completed {
            status: TaskStatus::Done,
            evidence_commit: evidence.clone(),
        }
    );
    assert_eq!(
        fixture_git(&repo, &["rev-parse", "refs/remotes/origin/agent-main"]),
        evidence,
        "the refresh fetch updates the exact remote-tracking ref"
    );

    fixture_git(
        &repo,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "not pushed to the remote",
        ],
    );
    let off_remote = fixture_git(&repo, &["rev-parse", "HEAD"]);
    let task = failed_task(&runtime, "Refuse a commit absent from the remote");
    let mut decision_request = request(&runtime, &task, &repo, FinalRecoveryCompletion::Done);
    decision_request.base_ref = "origin/agent-main".to_string();
    let outcome = apply(
        &runtime,
        &decision_request,
        json!({"decision": "complete_no_diff", "evidence_commit": off_remote, "rationale": "not on the remote"}),
    );
    let FinalRecoveryOutcome::Escalated {
        reason: Some(reason),
    } = outcome
    else {
        panic!("an unpushed commit must still escalate, got {outcome:?}");
    };
    assert!(reason.contains("was refreshed by one fetch"), "{reason}");
    assert_eq!(status(&runtime, &task.id), TaskStatus::Blocked);
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
            &request(&runtime, &task, repo, FinalRecoveryCompletion::Done),
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
        ..request(
            &runtime,
            current,
            root.path(),
            FinalRecoveryCompletion::Done,
        )
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
    let observed = request(&runtime, &task, root.path(), FinalRecoveryCompletion::Done);
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
        &request(
            &runtime,
            &archived,
            root.path(),
            FinalRecoveryCompletion::Done,
        ),
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
                &request(&runtime, &task, root.path(), FinalRecoveryCompletion::Done),
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
        &request(&runtime, &task, root.path(), FinalRecoveryCompletion::Done),
        json!({"decision": "resume", "step_id": "commit", "rationale": "conflict resolved"}),
    );
    assert_eq!(
        outcome,
        FinalRecoveryOutcome::Resume {
            step_id: "commit".to_string()
        }
    );
    assert_eq!(
        runtime
            .final_recovery_revision(&runtime.get_task(&task.id).expect("read task"))
            .expect("read revision"),
        runtime
            .final_recovery_revision(&task)
            .expect("read revision"),
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
        "orbit.workflow.run.resume",
        "orbit.workflow.run.show",
        "orbit.workflow.run.list",
        "orbit.workflow.ship",
        "orbit.task.add",
        "orbit.task.update",
        "orbit.task.artifact.put",
        "orbit.agent.invoke",
        "orbit.pipeline.invoke",
        "orbit.command.exec",
        "orbit.auto_task.add",
        "orbit.auto_task.mint",
        "orbit.auto_task.update",
    ] {
        assert!(
            denied.iter().any(|entry| entry == tool),
            "final recovery decides; the applier and engine act, so `{tool}` stays denied"
        );
    }
}
