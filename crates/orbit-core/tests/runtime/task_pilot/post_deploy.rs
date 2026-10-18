//! A pilot's typed `post_deploy` finding holds no admission, and lets an
//! owner run's implementer hand that criterion off as owed after deploy: the
//! delivery settles in review, task show lists the owed criterion, and only
//! an operator's evidenced record of it lets completion reach `done`. An
//! owed claim for any other criterion is refused.

use chrono::Utc;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, TaskAddParams,
    TaskUpdateParams,
};
use orbit_core::{OrbitError, Task, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};
use orbit_types::desktop::{
    DesktopCriterionOutcome, DesktopReviewDecision, DesktopReviewVerdict, DesktopTaskOperation,
    DesktopTaskRequest,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRunState, OwedCriterion};
use serde_json::{Value, json};

use super::Workspace;

const HOST: &str = "dk-server-2";
const COMMAND: &str = "orbit-latency --samples 20 /api/workflows/auto/readiness";
const RUN: &str = "jrun-post-deploy-fixture";

fn backlog_task(workspace: &Workspace) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Readiness latency".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec![
                "The readiness handler issues one store read.".into(),
                "The deployed dashboard's readiness p95 is at most 150 ms.".into(),
            ],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

/// Apply one assessment carrying `post_deploy` through the routine
/// prepare/apply actions.
fn apply(workspace: &Workspace, task: &Task, post_deploy: Value) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [{
                "task_id": task.id,
                "context_files_after": ["file:README.md"],
                "disposition": "selectors", "recommended_crew": "fixture",
                "recommended_complexity": "low", "confidence": "high",
                "assessment_rationale": "README.md contains the affected material.",
                "validation_approach": "Exercise delivery settlement.",
                "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                "duplicate_of": null, "already_landed": null,
                "post_deploy": post_deploy,
            }]}],
        }),
    )
}

fn latency_after_deploy() -> Value {
    json!([{
        "criterion": 2, "host": HOST, "command": COMMAND,
        "evidence": "Only the deployed orbit-web on the host serves the live dashboard.",
    }])
}

fn owed(criterion: usize, host: &str) -> OwedCriterion {
    OwedCriterion {
        criterion,
        host: host.into(),
        command: COMMAND.into(),
        target: "p95 <= 150 ms".into(),
        candidate_evidence: "The candidate's handler issued one store read.".into(),
    }
}

fn in_review(workspace: &Workspace, task: &Task) {
    for status in [TaskStatus::InProgress, TaskStatus::Review] {
        workspace
            .runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
    }
}

fn complete(workspace: &Workspace, task: &Task) -> Value {
    orbit_engine::execute_deterministic_action(
        &workspace.runtime,
        "task_complete",
        &json!({}),
        &json!({"run_id": RUN, "task_id": task.id}),
        false,
        &Default::default(),
        None,
    )
    .unwrap()
}

fn show(workspace: &Workspace, task: &Task) -> Value {
    workspace
        .runtime
        .run_tool("orbit.task.show", json!({"id": task.id}))
        .unwrap()
}

fn record(workspace: &Workspace, task: &Task, comment: &str) -> Result<(), OrbitError> {
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                comment: Some(comment.into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .map(|_| ())
}

fn a_post_deploy_finding_is_recorded_without_holding_admission() {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    let applied = apply(&workspace, &task, latency_after_deploy());
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    let audit: Value =
        serde_json::from_str(comments.last().unwrap().receipt().unwrap().body).unwrap();
    assert_eq!(
        audit["post_deploy"]["requirements"],
        json!([{"criterion": 2, "host": HOST, "command": COMMAND}]),
        "{audit}"
    );
    assert!(audit["native_os_hold"].is_null(), "{audit}");
    let backlog = workspace.action("list_backlog_tasks", json!({"task_ids": [task.id]}));
    assert!(
        backlog["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "a post-deploy finding holds no admission: {backlog}"
    );

    // A finding that names no host or measurement command, or a criterion
    // the task lacks, is refused at apply.
    for malformed in [
        json!([{"criterion": 2, "command": COMMAND, "evidence": "Live latency."}]),
        json!([{"criterion": 2, "host": HOST, "evidence": "Live latency."}]),
        json!([{"criterion": 3, "host": HOST, "command": COMMAND, "evidence": "Live."}]),
    ] {
        let refused = apply(&workspace, &task, malformed.clone());
        assert_ne!(refused["status"], "succeeded", "{malformed}: {refused}");
    }
}

fn an_owed_post_deploy_criterion_settles_in_review_until_an_operator_records_it() {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    let applied = apply(&workspace, &task, latency_after_deploy());
    assert_eq!(applied["status"], "succeeded", "{applied}");
    in_review(&workspace, &task);

    // Only the criterion the pilot typed, on its host, may be owed. Every
    // other unmet criterion still fails the implementer's step.
    for refused in [vec![owed(1, HOST)], vec![owed(2, "another-host")]] {
        let error = RuntimeHost::admit_owed_criteria(&workspace.runtime, &task.id, RUN, &refused)
            .unwrap_err();
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    }
    assert!(show(&workspace, &task).get("owed_criteria").is_none());
    RuntimeHost::admit_owed_criteria(&workspace.runtime, &task.id, RUN, &[owed(2, HOST)]).unwrap();

    let completion = complete(&workspace, &task);
    assert_eq!(
        completion["owed_task_ids"],
        json!([task.id]),
        "{completion}"
    );
    assert_eq!(completion["completed_task_ids"], json!([]), "{completion}");
    let shown = show(&workspace, &task);
    assert_eq!(shown["status"], "review", "{shown}");
    assert_eq!(shown["owed_criteria"][0]["criterion"], 2, "{shown}");
    assert_eq!(shown["owed_criteria"][0]["state"], "owed", "{shown}");
    assert_eq!(shown["owed_criteria"][0]["host"], HOST, "{shown}");
    assert_eq!(shown["owed_criteria"][0]["command"], COMMAND, "{shown}");
    assert_eq!(
        shown["owed_criteria"][0]["target"], "p95 <= 150 ms",
        "{shown}"
    );

    // An agent's comment of the same shape records nothing; an operator's
    // record needs evidence and an owed criterion.
    workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                comment: Some("post-deploy: met 2\nThe agent says it is fast.".into()),
                ..Default::default()
            },
            Some("codex".into()),
            Some("gpt-6.1-sol".into()),
        )
        .unwrap();
    assert!(record(&workspace, &task, "post-deploy: met 2").is_err());
    assert!(record(&workspace, &task, "post-deploy: met 1\nMeasured.").is_err());
    assert_eq!(show(&workspace, &task)["owed_criteria"][0]["state"], "owed");
    assert_eq!(
        complete(&workspace, &task)["owed_task_ids"],
        json!([task.id])
    );

    record(
        &workspace,
        &task,
        "post-deploy: met 2\nMeasured p95 121 ms over 20 paced samples on the host.",
    )
    .unwrap();
    let shown = show(&workspace, &task);
    assert_eq!(shown["owed_criteria"][0]["state"], "met", "{shown}");
    assert_eq!(shown["owed_criteria"][0]["recorded_by"], "human:fixture");
    let completion = complete(&workspace, &task);
    assert_eq!(
        completion["completed_task_ids"],
        json!([task.id]),
        "{completion}"
    );
    assert_eq!(show(&workspace, &task)["status"], "done");
}

fn a_reused_candidate_keeps_its_previous_runs_post_deploy_obligation() {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    let applied = apply(&workspace, &task, latency_after_deploy());
    assert_eq!(applied["status"], "succeeded", "{applied}");
    in_review(&workspace, &task);
    RuntimeHost::admit_owed_criteria(
        &workspace.runtime,
        &task.id,
        "jrun-previous-delivery",
        &[owed(2, HOST)],
    )
    .unwrap();

    // candidate_resume may reuse the previous candidate without running an
    // implementer, so the fresh run has no owed event of its own.
    let completion = complete(&workspace, &task);
    assert_eq!(
        completion["owed_task_ids"],
        json!([task.id]),
        "completion must retain the reused candidate's unresolved obligation: {completion}"
    );
    assert_eq!(show(&workspace, &task)["status"], "review");

    record(&workspace, &task, "post-deploy: failed 2\np95 was 180 ms.").unwrap();
    assert_eq!(
        show(&workspace, &task)["owed_criteria"][0]["state"],
        "failed"
    );
    assert_eq!(
        complete(&workspace, &task)["owed_task_ids"],
        json!([task.id])
    );

    record(
        &workspace,
        &task,
        "post-deploy: met 2\np95 was 121 ms after repair.",
    )
    .unwrap();
    assert_eq!(
        complete(&workspace, &task)["completed_task_ids"],
        json!([task.id])
    );
}

fn post_deploy_operator_measurements_do_not_hold_candidate_admission() {
    let workspace = Workspace::new();
    let task = backlog_task(&workspace);
    let post_deploy = "After deploy, run the live evaluation with `orbit.pipeline.invoke`.";
    let task = workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                acceptance_criteria: Some(vec![
                    "The readiness handler issues one store read.".into(),
                    post_deploy.into(),
                ]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let applied = apply(&workspace, &task, latency_after_deploy());
    assert_eq!(applied["status"], "succeeded", "{applied}");
    RuntimeHost::admit_task_for_workflow(&workspace.runtime, &task.id, "worktree_setup")
        .expect("an operator measurement owed after deploy must not hold candidate admission");
    let backlog = workspace.action("list_backlog_tasks", json!({"task_ids": [task.id]}));
    assert!(
        backlog["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "{backlog}"
    );

    // The same operator tool in an ordinary criterion must still hold the
    // candidate: typing criterion 2 never exempts criterion 1. Successful
    // admission moved this task in progress; return it for reassessment.
    let task = workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                acceptance_criteria: Some(vec![
                    "Run the live evaluation with `orbit.pipeline.invoke`.".into(),
                    post_deploy.into(),
                ]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let applied = apply(&workspace, &task, latency_after_deploy());
    assert_eq!(applied["status"], "succeeded", "{applied}");
    let error =
        RuntimeHost::admit_task_for_workflow(&workspace.runtime, &task.id, "worktree_setup")
            .expect_err("an ordinary operator requirement must still hold candidate admission");
    assert!(
        error
            .to_string()
            .contains("criterion 1 requires `orbit.pipeline.invoke`"),
        "{error}"
    );
    let backlog = workspace.action("list_backlog_tasks", json!({"task_ids": [task.id]}));
    assert!(
        !backlog["task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(task.id)),
        "{backlog}"
    );
}

/// Every completion boundary must preserve an owed or failed measurement,
/// even when a successful stopped delivery and summary already prove the work.
#[derive(Clone, Copy, Debug)]
enum CompletionBoundary {
    AgentTool,
    Approval,
    Desktop,
    Activity,
    Automation,
    FinalRecovery,
}

fn stopped_delivery_owing_measurement(workspace: &Workspace) -> Task {
    let task = backlog_task(workspace);
    assert_eq!(
        apply(workspace, &task, latency_after_deploy())["status"],
        "succeeded"
    );
    in_review(workspace, &task);
    let run = workspace
        .jobs
        .insert_job_run("task_pr_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    workspace
        .jobs
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    workspace
        .jobs
        .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let task = workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                job_run_id: Some(Some(run.run_id.clone())),
                execution_summary: Some(
                    "Candidate delivered; host measurement remains owed.".into(),
                ),
                ..Default::default()
            },
            Some("codex".into()),
            Some("gpt-6.1-sol".into()),
        )
        .unwrap();
    RuntimeHost::admit_owed_criteria(&workspace.runtime, &task.id, &run.run_id, &[owed(2, HOST)])
        .unwrap();
    task
}

fn attempt_completion(
    workspace: &Workspace,
    task: &Task,
    boundary: CompletionBoundary,
) -> Result<(), OrbitError> {
    let runtime = &workspace.runtime;
    match boundary {
        CompletionBoundary::AgentTool => runtime
            .run_tool(
                "orbit.task.update",
                json!({
                    "id": task.id, "status": "done", "model": "gpt-6.1-sol",
                    "execution_summary": "Candidate delivered and reviewed.",
                    // Agent prose cannot replace the operator's measurement.
                    "comment": "post-deploy: met 2\nAn agent claims the target was met.",
                }),
            )
            .map(|_| ()),
        CompletionBoundary::Approval => runtime.approve_task(&task.id, None, None).map(|_| ()),
        CompletionBoundary::Desktop => {
            let mut session = ToolSessionContext::default();
            session
                .effective_capabilities
                .insert(McpCapability::Operator);
            let revision = runtime.desktop_task_snapshot(&task.id, &session)?.revision;
            runtime
                .desktop_task_write(
                    DesktopTaskRequest {
                        request_id: format!("complete-{revision}"),
                        operation: DesktopTaskOperation::Review {
                            id: task.id.clone(),
                            expected_revision: revision,
                            verdict: DesktopReviewVerdict {
                                decision: DesktopReviewDecision::Accept,
                                rationale: "Candidate reviewed.".into(),
                                criteria: task
                                    .acceptance_criteria
                                    .iter()
                                    .map(|criterion| DesktopCriterionOutcome {
                                        criterion: criterion.clone(),
                                        met: true,
                                        evidence: vec!["execution_summary".into()],
                                    })
                                    .collect(),
                                evidence: vec!["execution_summary".into()],
                                expected_run_id: task.job_run_id.clone(),
                                expected_head: None,
                            },
                            complete: true,
                        },
                    },
                    None,
                    None,
                    &session,
                )
                .map(|_| ())
        }
        CompletionBoundary::Activity => RuntimeHost::update_task_from_activity(
            runtime,
            &task.id,
            TaskActivityUpdate {
                status: TaskStatus::Done,
                expected_status: TaskStatus::Review,
                execution_summary: Some("Candidate delivered.".into()),
                comment: None,
                note: None,
                agent: None,
                model: None,
                calling_run_id: None,
            },
        )
        .map(|_| ()),
        CompletionBoundary::Automation => RuntimeHost::apply_task_automation_update(
            runtime,
            &task.id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .map(|_| ()),
        CompletionBoundary::FinalRecovery => {
            let current = runtime.get_task(&task.id)?;
            let request = FinalRecoveryRequest {
                task_id: task.id.clone(),
                run_id: task.job_run_id.clone().unwrap(),
                observed: runtime.final_recovery_revision(&current)?,
                repo_root: workspace.repo.clone(),
                base_ref: "main".into(),
                completion: FinalRecoveryCompletion::Done,
                requeue_bound: Default::default(),
            };
            let decision = json!({
                "decision": "complete_no_diff",
                "evidence_commit": workspace.git(&["rev-parse", "HEAD"]),
                "rationale": "Delivered on base.",
            });
            let outcome = runtime.apply_final_recovery(&request, Some(&decision))?;
            assert!(
                matches!(
                    outcome,
                    FinalRecoveryOutcome::Completed {
                        status: TaskStatus::Done,
                        ..
                    }
                ),
                "{outcome:?}"
            );
            Ok(())
        }
    }
}

fn completion_requires_operator_measurement(boundary: CompletionBoundary) {
    let workspace = Workspace::new();
    let task = stopped_delivery_owing_measurement(&workspace);
    for state in ["owed", "failed"] {
        if state == "failed" {
            record(&workspace, &task, "post-deploy: failed 2\np95 was 180 ms.").unwrap();
        }
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let error = attempt_completion(&workspace, &task, boundary)
            .expect_err("completion must preserve owed or failed criteria");
        assert!(
            matches!(error, OrbitError::PolicyDenied(_)),
            "{boundary:?}: {error}"
        );
        let shown = show(&workspace, &task);
        assert_eq!(shown["status"], "review", "{boundary:?}");
        assert_eq!(shown["owed_criteria"][0]["state"], state);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
        assert_eq!(
            workspace.runtime.get_task_comments(&task.id).unwrap(),
            comments
        );
    }
    record(
        &workspace,
        &task,
        "post-deploy: met 2\np95 was 121 ms after repair.",
    )
    .unwrap();
    attempt_completion(&workspace, &task, boundary).unwrap();
    assert_eq!(show(&workspace, &task)["status"], "done", "{boundary:?}");
    assert_eq!(show(&workspace, &task)["owed_criteria"][0]["state"], "met");
}

fn agent_done_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::AgentTool);
}
fn approval_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::Approval);
}
fn desktop_completion_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::Desktop);
}
fn activity_completion_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::Activity);
}
fn automation_completion_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::Automation);
}
fn final_recovery_completion_requires_operator_measurement() {
    completion_requires_operator_measurement(CompletionBoundary::FinalRecovery);
}

fn human_force_completion_is_audited_without_fabricating_a_measurement() {
    let workspace = Workspace::new();
    let task = stopped_delivery_owing_measurement(&workspace);
    assert!(
        workspace
            .runtime
            .run_tool(
                "orbit.task.update",
                json!({
                    "id": task.id, "status": "done", "model": "gpt-6.1-sol", "force": true,
                })
            )
            .is_err(),
        "agents cannot request the human force override"
    );
    assert!(
        workspace
            .runtime
            .force_update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
                Some("codex".into()),
                Some("gpt-6.1-sol".into())
            )
            .is_err(),
        "an agent identity cannot bypass the measurement precondition"
    );
    workspace
        .runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(show(&workspace, &task)["status"], "done");
    assert_eq!(show(&workspace, &task)["owed_criteria"][0]["state"], "owed");
    let history = workspace.runtime.get_task_history(&task.id).unwrap();
    assert!(history.iter().any(|entry| entry.event == "forced"
        && entry.from_status == Some(TaskStatus::Review)
        && entry.to_status == Some(TaskStatus::Done)));
}

crate::scenario_table!(
    completion_requires_operator_measurement_scenarios,
    agent_done_requires_operator_measurement,
    approval_requires_operator_measurement,
    desktop_completion_requires_operator_measurement,
    activity_completion_requires_operator_measurement,
    automation_completion_requires_operator_measurement,
    final_recovery_completion_requires_operator_measurement,
    human_force_completion_is_audited_without_fabricating_a_measurement
);

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_post_deploy_finding_is_recorded_without_holding_admission_scenarios,
    a_post_deploy_finding_is_recorded_without_holding_admission,
    an_owed_post_deploy_criterion_settles_in_review_until_an_operator_records_it,
    a_reused_candidate_keeps_its_previous_runs_post_deploy_obligation,
    post_deploy_operator_measurements_do_not_hold_candidate_admission
);
