//! Fixtures and action drivers shared by the workspace auto-drain tests.

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::seed_list_backlog_task;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

pub(super) fn classify(runtime: &OrbitRuntime) -> Value {
    classify_with(runtime, json!({}))
}

pub(super) fn classify_with(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("classify workspace auto tasks")
}

/// A live `task_auto_pipeline` run carrying `task_ids`, as `invoke_detached`
/// leaves one behind.
pub(super) fn seed_live_leaf_run(runtime: &OrbitRuntime, task_ids: &[&str]) -> String {
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": task_ids })),
            None,
        )
        .expect("insert live leaf run")
        .run_id
}

pub(super) fn readiness(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    concurrency: Option<u32>,
) -> Value {
    readiness_allowing(runtime, task_ids, concurrency, &[])
}

pub(super) fn readiness_allowing(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    concurrency: Option<u32>,
    allowed_crews: &[String],
) -> Value {
    runtime
        .workspace_auto_readiness(task_ids, concurrency, 50, allowed_crews)
        .expect("explain readiness")
}

pub(super) fn readiness_task<'a>(output: &'a Value, task_id: &str) -> &'a Value {
    output["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .find(|task| task["task_id"] == task_id)
        .expect("readiness task")
}

/// Two crews plus a `system` entry that mirrors `opus` exactly — the shape that
/// makes "a wrapper is not provider usage" testable: `system` is a different
/// registry name for the same effective `(provider, model)`.
pub(super) const ALLOWLIST_CREW_CONFIG: &str = r#"
[workflow]
default_crew = "opus"
system_crew = "system"

[crews.opus]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"

[crews.fable]
provider = "claude"
model = "claude-fable-5-1"
backend = "cli"

[crews.system]
provider = "claude"
model = "claude-opus-4-6"
backend = "cli"
"#;

pub(super) fn seed_crewed_backlog_task(runtime: &OrbitRuntime, title: &str, crew: &str) -> String {
    let task = seed_list_backlog_task(
        runtime,
        title,
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                crew: Some(Some(crew.to_string())),
                ..Default::default()
            },
        )
        .expect("assign task crew");
    task.id
}

// [ORB-11253] A live worker ceiling, observed by the admission path.

/// A running drain with checkpoint state, as the engine leaves one behind.
pub(super) fn seed_running_drain(runtime: &OrbitRuntime, submitted: u32) -> String {
    seed_running_drain_input(runtime, json!({ "max_active_leaf_runs": submitted }))
}

pub(super) fn seed_running_drain_input(runtime: &OrbitRuntime, input: Value) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .expect("insert drain run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start drain run");
    let state = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".to_string(),
        input,
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("write drain state");
    run.run_id
}

pub(super) fn set_worker_limit(runtime: &OrbitRuntime, run_id: &str, concurrency: u32) {
    runtime
        .set_drain_worker_limit(crate::application::job::DrainWorkerLimitRequest {
            run_id,
            max_active_leaf_runs: concurrency,
            expected_revision: None,
            reason: None,
            actor: "tester",
            source: "unit",
            claim_token: None,
        })
        .expect("set worker limit");
}

pub(super) fn seed_backlog_leaves(runtime: &OrbitRuntime, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            seed_list_backlog_task(
                runtime,
                &format!("leaf {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec![&format!("crates/leaf_{index}/src/lib.rs")],
            )
            .id
        })
        .collect()
}

pub(super) fn seed_unassessed_task(runtime: &OrbitRuntime, title: &str, tags: &[&str]) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture outcome is observable.".to_string()],
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            plan: "Fixture plan.".to_string(),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed unassessed task")
}
