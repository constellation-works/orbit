//! The implementer envelope's comment budget keeps human and orchestrator
//! comments ahead of task-pilot assessments: a re-piloted task's superseded
//! assessment JSON must not push steering out of the agent's context, and
//! only the latest assessment is carried, as its summary line [ORB-23676].

use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_engine::RuntimeHost;
use serde_json::{Value, json};
use tempfile::TempDir;

fn pilot_comment(runtime: &OrbitRuntime, task: &str, rationale: &str) {
    let assessment = json!({
        "disposition": "selectors", "confidence": "high", "recommended_crew": "fixture-crew",
        "recommended_complexity": "medium",
        "assessment_rationale": rationale,
        "padding": "x".repeat(6 * 1024),
    });
    let message = format!(
        "operation_id={}\n{}",
        "a".repeat(64),
        json!({ "assessment": assessment })
    );
    comment_as(runtime, task, &message, "task-pilot");
}

fn comment_as(runtime: &OrbitRuntime, task: &str, message: &str, actor: &str) {
    runtime
        .update_task_as_human(
            task,
            TaskUpdateParams {
                comment: Some(message.into()),
                ..Default::default()
            },
            actor.into(),
        )
        .unwrap();
}

#[test]
fn human_comments_outrank_superseded_pilot_assessments_in_the_comment_budget() {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Budget fixture".into(),
            description: "A task re-piloted several times.".into(),
            acceptance_criteria: vec!["Human comments survive.".into()],
            ..Default::default()
        })
        .unwrap()
        .id;

    comment_as(
        &runtime,
        &task,
        "HUMAN-OLDEST: scope decision.",
        "human:operator",
    );
    pilot_comment(&runtime, &task, "first pass");
    comment_as(&runtime, &task, "ORCH: refinement.", "orchestrator");
    pilot_comment(&runtime, &task, "second pass");
    comment_as(&runtime, &task, "HUMAN-NEWEST: steering.", "human:operator");
    pilot_comment(&runtime, &task, "latest pass");

    let context = RuntimeHost::task_context_for_agent_input(
        &runtime,
        "run",
        "step",
        &json!({ "task_id": task }),
    )
    .unwrap()
    .expect("a task context");
    let messages: Vec<&str> = context["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|comment| comment["message"].as_str().unwrap())
        .collect();

    assert_eq!(messages.len(), 4, "{messages:?}");
    assert!(messages[0].contains("HUMAN-OLDEST"), "{messages:?}");
    assert!(messages[1].contains("ORCH"), "{messages:?}");
    assert!(messages[2].contains("HUMAN-NEWEST"), "{messages:?}");
    assert!(
        messages[3].starts_with("selectors (high); crew fixture-crew")
            && messages[3].contains("latest pass")
            && !messages[3].contains("assessment"),
        "the latest pilot comment is its summary line: {:?}",
        messages[3]
    );
    assert_eq!(context["comments_omitted_count"], 2);
    assert_eq!(context["comments_truncated"], Value::Bool(true));
}
