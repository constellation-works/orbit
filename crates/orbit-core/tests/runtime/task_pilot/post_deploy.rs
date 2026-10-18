//! A pilot's typed `post_deploy` finding holds no admission, and lets an
//! owner run's implementer hand that criterion off as owed after deploy: the
//! delivery settles in review, task show lists the owed criterion, and only
//! an operator's evidenced record of it lets completion reach `done`. An
//! owed claim for any other criterion is refused.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitError, Task, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::OwedCriterion;
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

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_post_deploy_finding_is_recorded_without_holding_admission_scenarios,
    a_post_deploy_finding_is_recorded_without_holding_admission,
    an_owed_post_deploy_criterion_settles_in_review_until_an_operator_records_it
);
