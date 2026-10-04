//! Final recovery through failing engine execution and real runtime cleanup.
//! [ORB-13994] The run failure must preserve its own requeue while ordinary
//! backlog tasks and the other recovery outcomes keep their behavior.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RuntimeHost, TaskAutomationUpdate, V2AuditWriter,
    WORKFLOW_RUN_FAILED_EVENT, execute_job_with_resume,
};
use orbit_store::contracts::JobRunStepParams;
use orbit_tools::ToolContext;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::activity_job::{JobV2, V2AuditEventKind};
use orbit_types::workflow::{FinalRecoveryDecision, JobRunState, JobTargetType};
use serde_json::{Value, json};

use super::{Fixture, fixture};

/// Script only the activity results; admission, application, checkpoints and
/// failed-run cleanup use the composed runtime and its real stores.
struct FailingPipeline<'a> {
    fixture: &'a Fixture,
    run: &'a str,
    task: &'a str,
    decision: Value,
    failure_status: TaskStatus,
}

impl RuntimeHost for FailingPipeline<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _context: ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "setup" => {
                self.fixture
                    .runtime
                    .apply_task_automation_update(
                        self.task,
                        TaskAutomationUpdate {
                            status: Some(TaskStatus::InProgress),
                            job_run_id: Some(self.run.to_string()),
                            ..Default::default()
                        },
                    )
                    .map_err(|error| DispatchError::JobExecution(error.to_string()))?;
                Ok(json!({"workspace_path": self.fixture.repo, "base_ref": "main"}))
            }
            "implement" => {
                // A task may have returned to backlog before the failure.
                self.fixture
                    .runtime
                    .apply_task_automation_update(
                        self.task,
                        TaskAutomationUpdate {
                            status: Some(self.failure_status),
                            ..Default::default()
                        },
                    )
                    .map_err(|error| DispatchError::JobExecution(error.to_string()))?;
                Err(DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "injected implementation failure".to_string(),
                })
            }
            "decide" => Ok(self.decision.clone()),
            "handoff" => Ok(json!({"blocked": true})),
            _ => panic!("unexpected fixture action: {action}"),
        }
    }

    fn checkpoint_step(
        &self,
        run_id: &str,
        index: u32,
        step_id: &str,
        output: &Value,
        compound: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        self.fixture
            .runtime
            .checkpoint_step(run_id, index, step_id, output, compound)
    }

    fn admit_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.fixture.runtime.admit_final_recovery(run_id, request)
    }

    fn apply_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        RuntimeHost::apply_final_recovery(&self.fixture.runtime, run_id, application)
    }
}

fn failing_job() -> JobV2 {
    let step =
        |id: &str| json!({"id": id, "spec": {"type": "deterministic", "action": id, "config": {}}});
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "final_recovery_fixture"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [step("setup"), step("implement")],
            },
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    let activity = |name| {
        load_activity_asset(
            &json!({
                "schemaVersion": 2,
                "kind": "Activity",
                "metadata": {"name": name},
                "spec": {"type": "deterministic", "description": name, "action": name, "config": {}},
            })
            .to_string(),
        )
        .unwrap()
        .spec
    };
    job.failure_activity = Some("handoff".to_string());
    job.resolved_failure_activity = Some(activity("handoff"));
    job.final_recovery_activity = Some("decide".to_string());
    job.resolved_final_recovery_activity = Some(activity("decide"));
    job
}

/// Fail a real engine step, apply its final decision and terminalize through
/// Core's ordinary cleanup. Return the task's snapshot before cleanup so the
/// assertions detect any later status, history or comment write.
fn fail_pipeline(
    fixture: &Fixture,
    run: &str,
    task: &str,
    decision: Value,
    completion: &str,
    failure_status: TaskStatus,
) -> (Value, Vec<(String, Option<String>)>) {
    let audit = V2AuditWriter::with_disk_sinks(
        &fixture.repo.join("audit"),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_fixture",
        run,
        "fixture",
        Some(&fixture.repo),
    )
    .unwrap();
    let result = execute_job_with_resume(
        &failing_job(),
        json!({"task_ids": [task], "completion": completion}),
        run,
        audit.clone(),
        &FailingPipeline {
            fixture,
            run,
            task,
            decision,
            failure_status,
        },
        None,
    );
    assert!(
        !result.as_ref().is_ok_and(|outcome| outcome.success),
        "the implementation step failed: {result:?}"
    );
    let before = fixture
        .runtime
        .run_tool("orbit.task.show", json!({"id": task}))
        .unwrap();
    let now = Utc::now();
    fixture
        .runtime
        .complete_job_run_step(
            run,
            &JobRunStepParams {
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "implement".to_string(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".to_string()),
                error_message: Some("injected implementation failure".to_string()),
            },
        )
        .unwrap();
    fixture
        .runtime
        .finalize_job_run(run, JobRunState::Failed, now, Some(1))
        .unwrap();
    assert_eq!(
        fixture.jobs.get_job_run(run).unwrap().unwrap().state,
        JobRunState::Failed
    );
    let attempts = audit
        .events_snapshot()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind {
            V2AuditEventKind::FinalRecoveryAttempted {
                outcome, decision, ..
            } => Some((outcome, decision)),
            _ => None,
        })
        .collect();
    (before, attempts)
}

#[test]
fn a_requeue_survives_failed_run_terminalization_even_if_the_outcome_write_fails() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::terminalization::a_requeue_survives_failed_run_terminalization_even_if_the_outcome_write_fails",
    ) {
        return;
    }
    for job in ["task_pr_pipeline", "task_local_pipeline"] {
        for failure_status in [TaskStatus::InProgress, TaskStatus::Backlog] {
            for lose_outcome in [false, true] {
                let fixture = fixture("[\"sol\"]");
                let task = fixture.task();
                let run = fixture.pipeline_run(&task, job);
                let fault = lose_outcome.then(|| fixture.fail_outcome_writes());
                let (before, attempts) = fail_pipeline(
                    &fixture,
                    &run,
                    &task,
                    json!({"decision": "requeue", "reason": "the environment is repaired"}),
                    "review",
                    failure_status,
                );
                drop(fault);
                assert_eq!(before["status"], "backlog");
                assert_eq!(
                    fixture.status(&task),
                    "backlog",
                    "the run failure must preserve its requeue decision [ORB-13994]: {job}, lose_outcome={lose_outcome}"
                );
                let after = fixture
                    .runtime
                    .run_tool("orbit.task.show", json!({"id": task}))
                    .unwrap();
                for field in ["status", "history", "comments", "updated_at", "job_run_id"] {
                    assert_eq!(after[field], before[field], "cleanup changed {field}");
                }
                assert_eq!(after["job_run_id"], run, "keep the run link for diagnosis");
                let history = fixture.runtime.get_task_history(&task).unwrap();
                let requeues: Vec<_> = history
                    .iter()
                    .filter(|entry| entry.event == "final_recovery_requeued")
                    .collect();
                assert_eq!(requeues.len(), 1);
                let transition = failure_status != TaskStatus::Backlog;
                assert_eq!(
                    requeues[0].from_status,
                    transition.then_some(failure_status)
                );
                assert_eq!(
                    requeues[0].to_status,
                    transition.then_some(TaskStatus::Backlog)
                );
                assert!(
                    history
                        .iter()
                        .all(|entry| entry.event != WORKFLOW_RUN_FAILED_EVENT)
                );
                assert_eq!(fixture.decision_comments(&task), 1);
                assert_eq!(
                    attempts,
                    [("settled".to_string(), Some("requeue".to_string()))]
                );
                assert_eq!(
                    fixture
                        .state(&run)
                        .final_recovery
                        .unwrap()
                        .outcome
                        .is_none(),
                    lose_outcome,
                    "the fault must exercise settlement without a durable run outcome"
                );
            }
        }
    }
}

#[test]
fn other_final_recovery_outcomes_survive_failed_run_terminalization() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::terminalization::other_final_recovery_outcomes_survive_failed_run_terminalization",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let git = |args: &[&str]| {
        let output = orbit_common::fs::git::run_git(&fixture.repo, args).unwrap();
        assert!(output.success, "git {args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&[
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@orbit.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        "landed",
    ]);
    let commit = git(&["rev-parse", "HEAD"]);
    let complete = json!({"decision": "complete_no_diff", "evidence_commit": commit, "rationale": "already landed"});
    for job in ["task_pr_pipeline", "task_local_pipeline"] {
        for (decision, completion, status, event, outcome) in [
            (
                json!({"decision": "escalate", "diagnosis": "provider denied access", "human_action": "restore access"}),
                "review",
                "blocked",
                "final_recovery_escalated",
                "escalated",
            ),
            (
                json!({"decision": "reject", "reason": "invalid requirement", "evidence": "the contract excludes it"}),
                "review",
                "rejected",
                "final_recovery_rejected",
                "settled",
            ),
            (
                json!({"decision": "archive", "reason": "superseded"}),
                "review",
                "archived",
                "final_recovery_archived",
                "settled",
            ),
            (
                complete.clone(),
                "review",
                "review",
                "final_recovery_completed",
                "settled",
            ),
            (
                complete.clone(),
                "done",
                "done",
                "final_recovery_completed",
                "settled",
            ),
        ] {
            let kind = decision["decision"].as_str().unwrap().to_string();
            let task = fixture.task();
            let run = fixture.pipeline_run(&task, job);
            let (before, attempts) = fail_pipeline(
                &fixture,
                &run,
                &task,
                decision,
                completion,
                TaskStatus::InProgress,
            );
            assert_eq!(fixture.status(&task), status, "{job}: {kind}, {completion}");
            let after = fixture
                .runtime
                .run_tool("orbit.task.show", json!({"id": task}))
                .unwrap();
            for field in ["status", "history", "comments", "updated_at"] {
                assert_eq!(
                    after[field], before[field],
                    "{kind}: cleanup changed {field}"
                );
            }
            let history = fixture.runtime.get_task_history(&task).unwrap();
            assert_eq!(
                history.iter().filter(|entry| entry.event == event).count(),
                1
            );
            assert!(
                history
                    .iter()
                    .all(|entry| entry.event != WORKFLOW_RUN_FAILED_EVENT)
            );
            assert_eq!(attempts, [(outcome.to_string(), Some(kind))]);
        }
    }
}

#[test]
fn failure_cleanup_still_blocks_backlog_without_a_current_requeue_from_that_run() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::terminalization::failure_cleanup_still_blocks_backlog_without_a_current_requeue_from_that_run",
    ) {
        return;
    }
    for case in [
        "ordinary backlog",
        "another run requeued",
        "later status transition",
    ] {
        let fixture = fixture("[\"sol\"]");
        let task = fixture.task();
        let mut run = fixture.running_run(&task);
        let couple = |run: &str, status| {
            fixture
                .runtime
                .apply_task_automation_update(
                    &task,
                    TaskAutomationUpdate {
                        job_run_id: Some(run.to_string()),
                        status: Some(status),
                        ..Default::default()
                    },
                )
                .unwrap();
        };
        couple(&run, TaskStatus::InProgress);
        if case != "ordinary backlog" {
            assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
            fixture.apply(
                &run,
                &task,
                FinalRecoveryDecision::Requeue {
                    reason: "retry after the environment repair".to_string(),
                },
                None,
            );
            if case == "another run requeued" {
                run = fixture.running_run(&task);
            } else {
                couple(&run, TaskStatus::InProgress);
            }
        }
        couple(&run, TaskStatus::Backlog);
        fixture
            .runtime
            .finalize_job_run(&run, JobRunState::Failed, Utc::now(), Some(1))
            .unwrap();
        assert_eq!(fixture.status(&task), "blocked", "{case}");
        let history = fixture.runtime.get_task_history(&task).unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == WORKFLOW_RUN_FAILED_EVENT)
                .count(),
            1,
            "{case}"
        );
    }
}
