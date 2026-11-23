//! An operator's explicit ship overrides the drain's readiness gates and
//! context-lock waits [ORB-25082].
//!
//! Readiness gates are drain policy: the drain, and a drain child naming a
//! task, still apply them. The operator's ship (CLI and dashboard) submits
//! the task anyway, marks its run so the listing and gate steps skip them,
//! and records each overridden gate and lock holder in the task's history.
//! The agent-facing tool does not override.

use std::collections::BTreeSet;

use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    CompletionPolicy, OrbitError, OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus,
};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

const CONFIG: &str = "[crews.coders]\nmodel = \"code-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"coders\"\n";
const FUTURE: &str = "2099-01-01T09:00:00Z";
const OPERATOR: &str = "operator:fixture";

fn task(
    runtime: &OrbitRuntime,
    title: &str,
    complexity: TaskComplexity,
    context_files: &[&str],
    status: TaskStatus,
    not_before: Option<&str>,
) -> Task {
    for selector in context_files {
        let file = selector.trim_start_matches("file:");
        std::fs::write(runtime.paths().repo_root.join(file), "fixture\n").unwrap();
    }
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            plan: "Fixture plan.".to_string(),
            complexity,
            context_files: context_files.iter().map(|s| s.to_string()).collect(),
            status: Some(status),
            not_before: not_before.map(|time| {
                chrono::DateTime::parse_from_rfc3339(time)
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            }),
            ..Default::default()
        })
        .expect("seed task")
}

fn deterministic(runtime: &OrbitRuntime, action: &str, input: Value) -> Value {
    runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

fn admitted(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_string())
        .collect()
}

fn excluded_reason(output: &Value, task: &str) -> Value {
    output["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == task)
        .map(|entry| entry["reason"].clone())
        .unwrap_or(Value::Null)
}

fn ship(runtime: &OrbitRuntime, task: &str, operator_override: bool) -> Result<(), OrbitError> {
    // The fixture's worker cannot start, so an admitted submission persists
    // its run and then reports the failed spawn.
    match runtime.submit_ship_run(orbit_core::ShipRunRequest {
        mode: ShipMode::Local,
        base_branch: Some("main"),
        task_ids: &[task.to_string()],
        completion: CompletionPolicy::Review,
        allowed_crews: &[],
        actor: Some(OPERATOR),
        claim_token: None,
        trigger: JobRunTrigger::cli(),
        strict_containment: false,
        review: None,
        operator_override,
        ignore_dependency_delivery: false,
    }) {
        Ok(_) | Err(OrbitError::Execution(_)) => Ok(()),
        Err(other) => Err(other),
    }
}

/// `orbit.workflow.ship` as an operator session calls it.
fn ship_tool(runtime: &OrbitRuntime, task: &str) -> Result<Value, OrbitError> {
    runtime.run_tool_with_context(
        "orbit.workflow.ship",
        json!({"task_ids": [task], "mode": "local"}),
        ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        },
    )
}

fn overrides(runtime: &OrbitRuntime, task: &str) -> Vec<String> {
    runtime
        .get_task_history(task)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.event == "readiness_overridden")
        .map(|entry| entry.note.unwrap_or_default())
        .collect()
}

/// A task scheduled ahead, with unassessed complexity and no context files,
/// is withheld by the drain and by a drain child naming it, and refused by
/// the agent tool. The operator's ship submits it, records each overridden
/// gate, and marks its run so the listing step admits it; without that mark
/// the gates still withhold it.
fn an_operator_ship_overrides_the_drains_readiness_gates() {
    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    let bare = task(
        &runtime,
        "unready",
        TaskComplexity::Unassessed,
        &[],
        TaskStatus::Backlog,
        Some(FUTURE),
    );

    let drain = deterministic(&runtime, "list_backlog_tasks", json!({}));
    assert_eq!(excluded_reason(&drain, &bare.id), "scheduled", "{drain}");
    let child = deterministic(
        &runtime,
        "list_backlog_tasks",
        json!({"task_ids": [bare.id]}),
    );
    assert_eq!(excluded_reason(&child, &bare.id), "scheduled", "{child}");
    let tool = ship_tool(&runtime, &bare.id).expect_err("the agent tool does not override");
    assert!(matches!(tool, OrbitError::TaskScheduled { .. }), "{tool:?}");
    let refused = ship(&runtime, &bare.id, false).expect_err("no override, no ship");
    assert!(
        matches!(refused, OrbitError::TaskScheduled { .. }),
        "{refused:?}"
    );

    ship(&runtime, &bare.id, true).expect("the operator's ship admits the task");
    let runs = runtime.list_job_runs(JobRunListParams::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].input.as_ref().unwrap()["operator_readiness_override"],
        true,
        "{:?}",
        runs[0].input
    );
    assert_eq!(
        overrides(&runtime, &bare.id),
        vec![format!(
            "operator `{OPERATOR}` shipped it explicitly, overriding the drain's readiness \
             gates: scheduled, unassessed_complexity, unknown_footprint"
        )]
    );

    let marked = deterministic(
        &runtime,
        "list_backlog_tasks",
        json!({"task_ids": [bare.id], "operator_readiness_override": true}),
    );
    assert_eq!(admitted(&marked), vec![bare.id.clone()], "{marked}");
    let child = deterministic(
        &runtime,
        "list_backlog_tasks",
        json!({"task_ids": [bare.id]}),
    );
    assert_eq!(
        excluded_reason(&child, &bare.id),
        "unassessed_complexity",
        "a drain child keeps the gate: {child}"
    );
    let drain = deterministic(&runtime, "list_backlog_tasks", json!({}));
    assert_eq!(
        excluded_reason(&drain, &bare.id),
        "unassessed_complexity",
        "the drain keeps the gate: {drain}"
    );
}

/// A gate carrying an operator's ship takes its reservation although another
/// in-flight task holds its context locks, holding no files and deferring
/// nothing to its leaf, and the task's history names the holder. A gate
/// without the mark still waits. Once the shipped task is `in-progress`, its
/// locks withhold overlapping backlog from the drain.
fn an_operator_ship_overrides_a_context_lock_conflict() {
    let (_root, runtime) = super::review_run_option::open(ShipMode::Local, CONFIG, true);
    let selector = "file:shared.txt";
    let holder = task(
        &runtime,
        "holder",
        TaskComplexity::Medium,
        &[selector],
        TaskStatus::InProgress,
        None,
    );
    let shipped = task(
        &runtime,
        "shipped",
        TaskComplexity::Medium,
        &[selector],
        TaskStatus::Backlog,
        None,
    );

    let waiting = deterministic(
        &runtime,
        "reserve_locks",
        json!({"task_ids": [shipped.id], "ttl_seconds": 120}),
    );
    assert_eq!(waiting["reserved"], false, "{waiting}");
    assert!(overrides(&runtime, &shipped.id).is_empty());

    let granted = deterministic(
        &runtime,
        "reserve_locks",
        json!({
            "task_ids": [shipped.id], "ttl_seconds": 120,
            "operator_readiness_override": true,
        }),
    );
    assert_eq!(granted["reserved"], true, "{granted}");
    assert_eq!(granted["footprint_deferred"], false, "{granted}");
    assert_eq!(granted["reserved_files"], json!([]), "{granted}");
    let notes = overrides(&runtime, &shipped.id);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains(&format!("context_lock_conflict (task {} on ", holder.id)),
        "the history names the holder: {notes:?}"
    );

    runtime
        .update_task_as_human(
            &shipped.id,
            orbit_core::application::task::TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            "fixture-operator".into(),
        )
        .expect("start the shipped task");
    let later = task(
        &runtime,
        "later",
        TaskComplexity::Medium,
        &[selector],
        TaskStatus::Backlog,
        None,
    );
    let drain = deterministic(&runtime, "list_backlog_tasks", json!({}));
    assert!(!admitted(&drain).contains(&later.id), "{drain}");
    let conflict = drain["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == later.id)
        .unwrap_or_else(|| panic!("the drain withholds overlapping work: {drain}"));
    assert_eq!(conflict["reason"], "context_lock_conflict", "{drain}");
    assert!(
        conflict["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["locking_task_id"] == shipped.id),
        "the shipped task's locks hold: {drain}"
    );
}

crate::scenario_table!(
    operator_ship_override_scenarios,
    an_operator_ship_overrides_the_drains_readiness_gates,
    an_operator_ship_overrides_a_context_lock_conflict
);
