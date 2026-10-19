//! Private final-recovery fault and replay seams; ordinary decisions live in runtime integration tests.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::git_test_support::run as run_git;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use orbit_types::record::FrictionStatus;
use orbit_types::task::{ExternalRef, Task, TaskRelation, TaskRelationType, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::tests::{enter_isolated_child, test_runtime};
use crate::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    TaskAddParams, TaskUpdateParams,
};
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
                        regression: None,
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
        super::super::apply::FAIL_SIDE_EFFECT_READ.with(|fail| fail.set(true));
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
