use std::collections::BTreeSet;

use chrono::{SecondsFormat, Utc};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_config, runtime_with_workspace_layout, seed_list_backlog_task,
    write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn classify(runtime: &OrbitRuntime) -> Value {
    classify_with(runtime, json!({}))
}

fn classify_with(runtime: &OrbitRuntime, input: Value) -> Value {
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
fn seed_live_leaf_run(runtime: &OrbitRuntime, task_ids: &[&str]) -> String {
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

fn drain_window(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic("drain_window", &json!({}), &input, ToolContext::default())
        .expect("drain window")
}

fn list_epic_descendants(runtime: &OrbitRuntime, epic_task_id: &str) -> Value {
    list_epic_descendants_with(runtime, epic_task_id, json!({}))
}

fn list_epic_descendants_with(runtime: &OrbitRuntime, epic_task_id: &str, extra: Value) -> Value {
    let mut input = extra;
    if let Some(object) = input.as_object_mut() {
        object.insert("epic_task_id".to_string(), json!(epic_task_id));
    }
    runtime
        .run_deterministic(
            "list_epic_descendants",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("list epic descendants")
}

fn list_epic_descendants_err(
    runtime: &OrbitRuntime,
    epic_task_id: &str,
    extra: Value,
) -> orbit_engine::DispatchError {
    let mut input = extra;
    if let Some(object) = input.as_object_mut() {
        object.insert("epic_task_id".to_string(), json!(epic_task_id));
    }
    runtime
        .run_deterministic(
            "list_epic_descendants",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect_err("list epic descendants should fail")
}

fn readiness(runtime: &OrbitRuntime, task_ids: &[String], concurrency: Option<u32>) -> Value {
    readiness_allowing(runtime, task_ids, concurrency, &[])
}

fn readiness_allowing(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    concurrency: Option<u32>,
    allowed_crews: &[String],
) -> Value {
    runtime
        .workspace_auto_readiness(task_ids, concurrency, 50, allowed_crews)
        .expect("explain readiness")
}

fn readiness_task<'a>(output: &'a Value, task_id: &str) -> &'a Value {
    output["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .find(|task| task["task_id"] == task_id)
        .expect("readiness task")
}

#[test]
fn readiness_explains_dependencies_locks_epics_claims_and_capacity() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/locked/src/lib.rs");
    let dependency = seed_list_backlog_task(
        &runtime,
        "Unfinished dependency",
        TaskStatus::Proposed,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let blocked = runtime
        .add_task(TaskAddParams {
            title: "Blocked leaf".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            dependencies: vec![dependency.id.clone()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed blocked leaf");
    let missing_dependency = seed_list_backlog_task(
        &runtime,
        "Deleted dependency",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let missing = runtime
        .add_task(TaskAddParams {
            title: "Missing dependency leaf".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            dependencies: vec![missing_dependency.id.clone()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed missing dependency leaf");
    runtime
        .delete_task(&missing_dependency.id)
        .expect("delete dependency for missing fixture");
    let _holder = seed_list_backlog_task(
        &runtime,
        "Lock holder",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/locked/src/lib.rs"],
    );
    let locked = seed_list_backlog_task(
        &runtime,
        "Locked leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/locked/src/lib.rs"],
    );
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Managed epic".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            tags: vec!["epic".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed epic");
    let epic_child = seed_list_backlog_task(
        &runtime,
        "Managed epic child",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(epic.id),
        vec![],
    );
    let claimed = seed_list_backlog_task(
        &runtime,
        "Claimed leaf",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let claim_run = seed_live_leaf_run(&runtime, &[&claimed.id]);
    let saturated = seed_list_backlog_task(
        &runtime,
        "Capacity leaf",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec![],
    );

    let ids = vec![
        blocked.id.clone(),
        missing.id.clone(),
        locked.id.clone(),
        epic_child.id.clone(),
        claimed.id.clone(),
        saturated.id.clone(),
    ];
    let output = readiness(&runtime, &ids, Some(1));

    assert_eq!(
        readiness_task(&output, &blocked.id)["reason"],
        "unmet_dependency"
    );
    assert_eq!(
        readiness_task(&output, &missing.id)["dependencies"][0]["status"],
        "missing"
    );
    assert_eq!(
        readiness_task(&output, &locked.id)["reason"],
        "context_lock_conflict"
    );
    assert_eq!(
        readiness_task(&output, &epic_child.id)["reason"],
        "epic_managed"
    );
    assert_eq!(
        readiness_task(&output, &claimed.id)["reason"],
        "claimed_by_live_child"
    );
    assert_eq!(
        readiness_task(&output, &claimed.id)["run_ids"],
        json!([claim_run])
    );
    assert_eq!(
        readiness_task(&output, &saturated.id)["reason"],
        "capacity_saturated"
    );
}

#[test]
fn readiness_matches_dispatch_and_does_not_mutate_the_snapshot() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let first = seed_list_backlog_task(
        &runtime,
        "First ready leaf",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let second = seed_list_backlog_task(
        &runtime,
        "Second ready leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let ids = vec![second.id.clone(), first.id.clone()];
    let before_runs = runtime
        .stores()
        .jobs()
        .list_pending_or_running_job_runs("task_auto_pipeline")
        .expect("list runs");

    let output = readiness(&runtime, &ids, Some(1));
    let dispatched = classify_with(&runtime, json!({ "max_active_leaf_runs": 1 }));

    assert_eq!(readiness_task(&output, &first.id)["reason"], "ready");
    assert_eq!(readiness_task(&output, &first.id)["eligible"], true);
    assert_eq!(
        readiness_task(&output, &second.id)["reason"],
        "capacity_saturated"
    );
    assert_eq!(dispatched["loose_task_ids"], json!([first.id]));
    assert_eq!(
        runtime
            .stores()
            .jobs()
            .list_pending_or_running_job_runs("task_auto_pipeline")
            .expect("list runs after"),
        before_runs
    );
    assert_eq!(
        runtime.get_task(&second.id).expect("read task").status,
        TaskStatus::Backlog
    );
    assert!(
        output["snapshot"]["limitations"]
            .as_str()
            .expect("limitations")
            .contains("does not guarantee")
    );
}

#[test]
fn epic_descendants_are_dependency_then_dispatch_ordered_and_terminal_tasks_are_skipped() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Epic root".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed epic root");
    let foundation = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Foundation".to_string(),
            description: "Foundation fixture".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            plan: "Implement".to_string(),
            priority: TaskPriority::Low,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed foundation");
    let dependent = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Dependent".to_string(),
            description: "Dependent fixture".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            dependencies: vec![foundation.id.clone()],
            plan: "Implement".to_string(),
            priority: TaskPriority::High,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed dependent");
    let independent = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Independent".to_string(),
            description: "Independent fixture".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            plan: "Implement".to_string(),
            priority: TaskPriority::Critical,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed independent");
    let corrective = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Corrective".to_string(),
            description: "Corrective fixture".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            plan: "Implement".to_string(),
            priority: TaskPriority::Low,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed corrective child");
    let done = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Already done".to_string(),
            description: "Done fixture".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            plan: "Implemented".to_string(),
            status: Some(TaskStatus::Done),
            ..Default::default()
        })
        .expect("seed done child");

    let output = list_epic_descendants(&runtime, &epic.id);
    assert_eq!(
        output["task_ids"],
        json!([independent.id, corrective.id, foundation.id, dependent.id])
    );
    assert_eq!(output["task_count"], 4);
    assert!(
        !output["task_ids"]
            .as_array()
            .expect("task ids")
            .contains(&json!(done.id))
    );
}

#[test]
fn epic_with_no_descendants_has_an_empty_drain() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Empty epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["No children".to_string()],
            tags: vec!["epic".to_string()],
            plan: "No-op".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed empty epic");

    let output = list_epic_descendants(&runtime, &epic.id);
    assert_eq!(output["task_ids"], json!([]));
    assert_eq!(output["task_count"], 0);
    assert_eq!(output["empty"], true);
}

#[test]
fn leftover_descendants_fail_closed_and_name_the_ids() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Epic root".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed epic root");
    let leftover = runtime
        .add_task(TaskAddParams {
            parent_id: Some(epic.id.clone()),
            title: "Still open".to_string(),
            description: "Unfinished descendant".to_string(),
            acceptance_criteria: vec!["Done".to_string()],
            plan: "Implement".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed leftover child");
    let unrelated = seed_list_backlog_task(
        &runtime,
        "Unrelated chore",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec![],
    );

    let error = list_epic_descendants_err(&runtime, &epic.id, json!({ "fail_if_nonempty": true }));
    match error {
        orbit_engine::DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, "list_epic_descendants");
            assert!(message.contains(&leftover.id), "{message}");
            assert!(
                message.contains("epic descendants remain after drain"),
                "{message}"
            );
            assert!(
                !message.contains(&unrelated.id),
                "unrelated backlog must not appear in the epic fail-closed message: {message}"
            );
        }
        other => panic!("expected leftover-descendant failure, got {other:?}"),
    }
}

#[test]
fn fail_if_nonempty_ignores_unrelated_backlog_when_the_epic_is_empty() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Empty epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["No children".to_string()],
            tags: vec!["epic".to_string()],
            plan: "No-op".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed empty epic");
    seed_list_backlog_task(
        &runtime,
        "Unrelated chore",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec![],
    );

    let output =
        list_epic_descendants_with(&runtime, &epic.id, json!({ "fail_if_nonempty": true }));
    assert_eq!(output["empty"], true);
    assert_eq!(output["task_ids"], json!([]));
}

#[test]
fn two_loose_tasks_and_one_epic_root_are_admissible_together() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let loose_one = seed_list_backlog_task(
        &runtime,
        "Loose high",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let loose_two = seed_list_backlog_task(
        &runtime,
        "Loose medium",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Epic root".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed epic root");
    for index in 0..3 {
        seed_list_backlog_task(
            &runtime,
            &format!("Epic child {index}"),
            TaskStatus::Backlog,
            TaskPriority::Medium,
            TaskType::Chore,
            Some(epic.id.clone()),
            vec![],
        );
    }

    // Leaves and the epic are independent answers, so both are admissible in
    // the same iteration: the drain ships the leaves and starts the epic.
    let first = classify(&runtime);
    assert_eq!(first["loose_task_ids"], json!([loose_one.id, loose_two.id]));
    assert_eq!(
        first["loose_task_dispatches"],
        json!([
            { "task_ids": [loose_one.id] },
            { "task_ids": [loose_two.id] },
        ])
    );
    assert_eq!(first["has_leaves"], true);
    assert_eq!(first["epic_task_id"], epic.id);
    assert_eq!(first["has_epic"], true);
    assert_eq!(first["idle"], false);

    for loose in [&loose_one, &loose_two] {
        runtime
            .update_task(
                &loose.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                },
            )
            .expect("complete loose task");
    }
    let second = classify(&runtime);
    assert_eq!(second["epic_task_id"], epic.id);
    assert_eq!(second["loose_task_ids"], json!([]));
    assert_eq!(second["has_leaves"], false);
}

#[test]
fn automatic_epic_root_choice_uses_the_shared_dispatch_order() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let high_feature = runtime
        .add_task(TaskAddParams {
            title: "High feature epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            priority: TaskPriority::High,
            task_type: Some(TaskType::Feature),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed high feature epic");
    let corrective = runtime
        .add_task(TaskAddParams {
            title: "Low security review epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string(), "security-review".to_string()],
            plan: "Delegate children".to_string(),
            priority: TaskPriority::Low,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed corrective epic");
    let critical = runtime
        .add_task(TaskAddParams {
            title: "Critical refactor epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            priority: TaskPriority::Critical,
            task_type: Some(TaskType::Refactor),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed critical epic");

    assert_eq!(classify(&runtime)["epic_task_id"], critical.id);

    runtime
        .update_task(
            &critical.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("complete critical epic");
    assert_eq!(classify(&runtime)["epic_task_id"], corrective.id);
    assert_ne!(classify(&runtime)["epic_task_id"], high_feature.id);
}

#[test]
fn loose_tasks_are_partitioned_by_effective_crew_in_priority_order() {
    let root = tempfile::tempdir().expect("create tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    std::fs::write(
        workspace.join("config.toml"),
        r#"
[workflow]
default_crew = "sol"

[crews.sol]
provider = "codex"
backend = "cli"
model = "gpt-5.6-sol"

[crews.terra]
provider = "codex"
backend = "cli"
model = "gpt-5.6-terra"
"#,
    )
    .expect("write crew fixture");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let sol_high = runtime
        .add_task(TaskAddParams {
            title: "Sol high".to_string(),
            description: "Crew partition fixture".to_string(),
            priority: TaskPriority::High,
            crew: Some("sol".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed sol task");
    let terra = runtime
        .add_task(TaskAddParams {
            title: "Terra medium".to_string(),
            description: "Crew partition fixture".to_string(),
            priority: TaskPriority::Medium,
            crew: Some("terra".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed terra task");
    let sol_low = runtime
        .add_task(TaskAddParams {
            title: "Sol low".to_string(),
            description: "Crew partition fixture".to_string(),
            priority: TaskPriority::Low,
            crew: Some("sol".to_string()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed second sol task");

    let output = classify(&runtime);
    assert_eq!(
        output["loose_task_ids"],
        json!([sol_high.id, terra.id, sol_low.id])
    );
    // One task per dispatch, so a child is crew-homogeneous by construction
    // rather than by partitioning. What still has to hold is that the child
    // resolves the crew of the task it was handed — that resolution, not
    // anything workspace-auto puts in the dispatch, is the fail-closed
    // authority.
    assert_eq!(
        output["loose_task_dispatches"],
        json!([
            { "task_ids": [sol_high.id] },
            { "task_ids": [terra.id] },
            { "task_ids": [sol_low.id] },
        ])
    );
    for (dispatch, expected_crew) in output["loose_task_dispatches"]
        .as_array()
        .expect("dispatches")
        .iter()
        .zip(["sol", "terra", "sol"])
    {
        let input = json!({ "task_ids": dispatch["task_ids"] });
        let run = runtime
            .stores()
            .jobs()
            .insert_job_run(
                "task_auto_pipeline",
                1,
                Utc::now(),
                Some(input.clone()),
                None,
            )
            .expect("insert homogeneous child");
        runtime
            .record_run_crew_from_input(&run.run_id, &input)
            .expect("persist homogeneous child crew");
        assert_eq!(
            runtime
                .show_job_run(&run.run_id)
                .expect("show homogeneous child")
                .resolved_crew
                .as_deref(),
            Some(expected_crew)
        );
    }
}

/// The `hold` decision this replaces froze every conflict-free chore for as
/// long as an epic root was `in-progress`. Admission is the epic's lock
/// reservation instead: the leaf that overlaps its descendants' declared files
/// is excluded, and the one that does not still ships in the same drain.
#[test]
fn a_live_epic_excludes_only_the_leaves_that_overlap_its_reservation() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/epic/src/lib.rs");
    write_workspace_file(&repo_root, "crates/elsewhere/src/lib.rs");
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Active epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            workspace_path: Some(".".to_string()),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed active epic");
    // The epic root reserves the union of its descendants' context files.
    seed_list_backlog_task(
        &runtime,
        "Epic child",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(epic.id.clone()),
        vec!["crates/epic/src/lib.rs"],
    );
    let overlapping = seed_list_backlog_task(
        &runtime,
        "Late loose task inside the epic's files",
        TaskStatus::Backlog,
        TaskPriority::Critical,
        TaskType::Chore,
        None,
        vec!["crates/epic/src/lib.rs"],
    );
    let conflict_free = seed_list_backlog_task(
        &runtime,
        "Late loose task elsewhere",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec!["crates/elsewhere/src/lib.rs"],
    );

    let admissible = classify(&runtime);
    assert_eq!(admissible["loose_task_ids"], json!([conflict_free.id]));
    assert_eq!(admissible["has_leaves"], true);
    assert_eq!(admissible["idle"], false);
    assert!(
        !admissible["loose_task_ids"]
            .as_array()
            .expect("loose task ids")
            .contains(&json!(overlapping.id)),
        "a leaf overlapping the epic's reserved files must not ship"
    );
}

#[test]
fn an_empty_workspace_is_admissibly_empty() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let quiet = classify(&runtime);

    assert_eq!(quiet["loose_task_ids"], json!([]));
    assert_eq!(quiet["has_leaves"], false);
    assert_eq!(quiet["epic_task_id"], Value::Null);
    assert_eq!(quiet["has_epic"], false);
    assert_eq!(quiet["idle"], true);
    assert_eq!(quiet["active_epic_run_id"], Value::Null);
}

#[test]
fn a_backlog_epic_root_waits_while_an_epic_run_is_live() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let waiting = runtime
        .add_task(TaskAddParams {
            title: "Second epic root".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Supervised".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Delegate children".to_string(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed backlog epic root");

    assert_eq!(classify(&runtime)["epic_task_id"], waiting.id);

    // `epic_pipeline` admits one active run. Once one is live, offering
    // another root would queue a pending run rather than start work — and the
    // drain loop would mint a fresh one every iteration.
    let live = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "epic_pipeline",
            1,
            Utc::now(),
            Some(json!({ "epic_task_id": "ORB-00001" })),
            None,
        )
        .expect("insert live epic run");

    let admissible = classify(&runtime);
    assert_eq!(admissible["epic_task_id"], Value::Null);
    assert_eq!(admissible["has_epic"], false);
    assert_eq!(admissible["idle"], true);
    assert_eq!(admissible["active_epic_run_id"], live.run_id);
    assert_eq!(admissible["active_epic_task_id"], "ORB-00001");
}

#[test]
fn an_absent_window_is_expired_on_its_first_answer() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    // `break_when` is evaluated after the loop body, so an already-expired
    // window still yields exactly one iteration — today's one-tick behavior.
    let stamped = drain_window(&runtime, json!({}));
    assert_eq!(stamped["expired"], true);
    assert_eq!(stamped["remaining_seconds"], 0.0);

    // The template over an absent `for_seconds` renders an empty string.
    let rendered_absent = drain_window(&runtime, json!({ "for_seconds": "" }));
    assert_eq!(rendered_absent["expired"], true);
}

#[test]
fn a_stamped_window_answers_expiry_against_its_own_deadline() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let stamped = drain_window(&runtime, json!({ "for_seconds": 600 }));
    assert_eq!(stamped["expired"], false);
    let remaining = stamped["remaining_seconds"]
        .as_f64()
        .expect("remaining seconds");
    assert!(
        (595.0..=600.0).contains(&remaining),
        "expected ~600s remaining, got {remaining}"
    );

    // Re-reading the stamp is a pure function of the deadline the first call
    // returned; nothing durable is written between the two.
    let reread = drain_window(&runtime, json!({ "deadline": stamped["deadline"] }));
    assert_eq!(reread["expired"], false);
    assert_eq!(reread["deadline"], stamped["deadline"]);

    let past =
        (Utc::now() - chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true);
    assert_eq!(
        drain_window(&runtime, json!({ "deadline": past }))["expired"],
        true
    );
}

#[test]
fn a_stopped_drain_expires_the_window_without_waiting_for_the_deadline() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let drain_run_id = seed_running_drain(&runtime, 5);
    runtime
        .stop_workspace_auto_admissions(crate::application::job::DrainAdmissionsStopRequest {
            actor: "tester",
            source: "unit",
            reason: None,
            claim_token: None,
        })
        .expect("stop drain");

    let stamped = drain_window(
        &runtime,
        json!({ "run_id": drain_run_id, "for_seconds": 600 }),
    );
    assert_eq!(stamped["expired"], true);
    assert_eq!(stamped["expired_reason"], "admissions_stopped");
    assert_eq!(stamped["remaining_seconds"], 0.0);
}

#[test]
fn a_drain_window_rejects_an_unparseable_deadline_or_an_oversize_request() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    for input in [
        json!({ "deadline": "not-a-timestamp" }),
        json!({ "for_seconds": 86_401 }),
        json!({ "for_seconds": -1 }),
    ] {
        assert!(
            runtime
                .run_deterministic("drain_window", &json!({}), &input, ToolContext::default())
                .is_err(),
            "expected {input} to be refused"
        );
    }
}

/// The drain no longer waits on its leaves, so the thing that bounds
/// parallelism is the number of live children rather than the size of a batch.
/// Only the free slots are offered, and they go to the front of the
/// priority/age queue rather than to whichever tasks happen to sort last.
#[test]
fn leaves_are_offered_only_up_to_the_free_leaf_run_slots() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let seeded: Vec<_> = (0..4)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Loose {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec![],
            )
        })
        .collect();

    let capped = classify_with(&runtime, json!({ "max_active_leaf_runs": 2 }));
    assert_eq!(
        capped["loose_task_ids"],
        json!([seeded[0].id, seeded[1].id]),
        "the two free slots go to the front of the queue"
    );
    assert_eq!(capped["free_slots"], 2);
    assert_eq!(capped["active_leaf_runs"], 0);
    assert_eq!(capped["pending_backlog"], 4);
    assert_eq!(capped["idle"], false);

    // One slot taken by a live child: one leaf offered, and never the task
    // that child is already carrying.
    seed_live_leaf_run(&runtime, &[seeded[0].id.as_str()]);
    let partial = classify_with(&runtime, json!({ "max_active_leaf_runs": 2 }));
    assert_eq!(partial["active_leaf_runs"], 1);
    assert_eq!(partial["free_slots"], 1);
    assert_eq!(partial["loose_task_ids"], json!([seeded[1].id]));
    assert_eq!(partial["pending_backlog"], 3);
}

/// A leaf handed to a detached child stays `backlog` until that child moves it
/// to `in-progress`. Without reading the child's own input, the very next
/// iteration would hand the same task to a second child.
#[test]
fn tasks_carried_by_a_live_child_are_never_offered_again() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let claimed = seed_list_backlog_task(
        &runtime,
        "Already dispatched",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let fresh = seed_list_backlog_task(
        &runtime,
        "Still waiting",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Chore,
        None,
        vec![],
    );
    seed_live_leaf_run(&runtime, &[claimed.id.as_str()]);

    let output = classify(&runtime);
    assert_eq!(output["loose_task_ids"], json!([fresh.id]));
    assert_eq!(
        output["pending_backlog"], 1,
        "the claimed task is not pending — it is running"
    );
    assert_eq!(
        claimed.status,
        TaskStatus::Backlog,
        "and it is still backlog"
    );
}

/// `idle` means "this iteration started nothing", which a saturated drain is
/// even with a full backlog behind it. The wait that follows has to tell the
/// two apart: a freed slot should be refilled in seconds, while an empty
/// workspace has nothing to poll for.
#[test]
fn saturation_waits_the_short_poll_and_an_empty_workspace_waits_the_long_one() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let waiting = seed_list_backlog_task(
        &runtime,
        "Queued behind a full slot table",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    seed_live_leaf_run(&runtime, &["ORB-SOMETHING-ELSE"]);

    let saturated = classify_with(
        &runtime,
        json!({
            "max_active_leaf_runs": 1,
            "poll_sleep_seconds": 7,
            "idle_sleep_seconds": 900,
        }),
    );
    assert_eq!(saturated["idle"], true, "nothing started");
    assert_eq!(saturated["free_slots"], 0);
    assert_eq!(saturated["pending_backlog"], 1, "but work is queued");
    assert_eq!(saturated["sleep_seconds"], 7);

    runtime
        .update_task(
            &waiting.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("drain the backlog");
    let quiet = classify_with(
        &runtime,
        json!({
            "max_active_leaf_runs": 1,
            "poll_sleep_seconds": 7,
            "idle_sleep_seconds": 900,
        }),
    );
    assert_eq!(quiet["idle"], true);
    assert_eq!(quiet["pending_backlog"], 0);
    assert_eq!(quiet["sleep_seconds"], 900);
}

/// Every loop input reaches this action through the template engine, which
/// renders a number as a string and an absent key as an empty one. Both must
/// land on the same value the literal would.
#[test]
fn loop_inputs_survive_template_rendering_as_strings() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    for index in 0..3 {
        seed_list_backlog_task(
            &runtime,
            &format!("Loose {index}"),
            TaskStatus::Backlog,
            TaskPriority::Medium,
            TaskType::Chore,
            None,
            vec![],
        );
    }

    let templated = classify_with(
        &runtime,
        json!({
            "max_active_leaf_runs": "2",
            "poll_sleep_seconds": "11",
            "idle_sleep_seconds": "",
        }),
    );
    assert_eq!(templated["free_slots"], 2);
    assert_eq!(
        templated["loose_task_dispatches"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(templated["sleep_seconds"], 11);

    let empty_string_falls_back = classify_with(&runtime, json!({ "max_active_leaf_runs": "" }));
    assert_eq!(empty_string_falls_back["free_slots"], 5);
}

/// Two crews plus a `system` entry that mirrors `opus` exactly — the shape that
/// makes "a wrapper is not provider usage" testable: `system` is a different
/// registry name for the same effective `(provider, model)`.
const ALLOWLIST_CREW_CONFIG: &str = r#"
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

fn seed_crewed_backlog_task(runtime: &OrbitRuntime, title: &str, crew: &str) -> String {
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

/// [ORB-11242] A restricted window skips the crews it excludes and keeps
/// draining everything else. The excluded task is left exactly as it is — still
/// `backlog`, still on its own crew — and readiness says so by name, which is
/// what makes "reassign it yourself" an instruction the operator can follow.
#[test]
fn crew_allowlist_skips_excluded_tasks_and_keeps_draining_the_rest() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(ALLOWLIST_CREW_CONFIG));
    let permitted = seed_crewed_backlog_task(&runtime, "Permitted leaf", "opus");
    let excluded = seed_crewed_backlog_task(&runtime, "Excluded leaf", "fable");

    let classified = classify_with(&runtime, json!({ "allowed_crews": ["opus"] }));
    assert_eq!(classified["loose_task_ids"], json!([permitted]));
    assert_eq!(classified["has_leaves"], json!(true));

    let readiness = readiness_allowing(&runtime, &[], None, &["opus".to_string()]);
    assert_eq!(readiness_task(&readiness, &permitted)["reason"], "ready");
    let blocked = readiness_task(&readiness, &excluded);
    assert_eq!(blocked["eligible"], json!(false));
    assert_eq!(blocked["reason"], "crew_not_allowed");
    assert_eq!(blocked["crew"], "fable");
    assert_eq!(blocked["allowed_crews"], json!(["opus"]));

    // The drain never rewrites the task it skipped.
    assert_eq!(
        runtime.get_task(&excluded).expect("excluded task").crew,
        Some("fable".to_string())
    );

    // Omitting the option is the pre-ORB-11242 behavior: both tasks admitted.
    let unrestricted = classify(&runtime);
    let admitted = unrestricted["loose_task_ids"]
        .as_array()
        .expect("loose task ids")
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        admitted,
        BTreeSet::from([permitted.as_str(), excluded.as_str()])
    );
}

/// A crew that resolves to the *same* configured provider/model as a permitted
/// one is permitted under its own name too: the allowlist restricts what runs,
/// not which alias names it.
#[test]
fn crew_allowlist_permits_an_alias_of_a_permitted_identity() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(ALLOWLIST_CREW_CONFIG));
    let aliased = seed_crewed_backlog_task(&runtime, "System-aliased leaf", "system");

    let classified = classify_with(&runtime, json!({ "allowed_crews": ["opus"] }));
    assert_eq!(classified["loose_task_ids"], json!([aliased]));
}

/// An epic root is admitted through the same effective-crew rule as a leaf, so
/// a restricted window cannot start one whose crew it excluded.
#[test]
fn crew_allowlist_withholds_an_excluded_epic_root() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(ALLOWLIST_CREW_CONFIG));
    let epic = seed_list_backlog_task(
        &runtime,
        "Excluded epic",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Feature,
        None,
        vec![],
    );
    runtime
        .update_task(
            &epic.id,
            TaskUpdateParams {
                crew: Some(Some("fable".to_string())),
                tags: Some(vec!["epic".to_string()]),
                ..Default::default()
            },
        )
        .expect("tag epic root");

    assert_eq!(
        classify_with(&runtime, json!({ "allowed_crews": ["opus"] }))["has_epic"],
        json!(false)
    );
    assert_eq!(classify(&runtime)["epic_task_id"], json!(epic.id));
}

/// The allowlist is validated where the operator can act on it, not silently
/// narrowed at dispatch time.
#[test]
fn crew_allowlist_rejects_a_crew_this_workspace_does_not_configure() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(ALLOWLIST_CREW_CONFIG));
    let error = runtime
        .workspace_auto_readiness(&[], None, 50, &["nope".to_string()])
        .expect_err("an unconfigured crew must fail");
    assert!(error.to_string().contains("nope"), "{error}");
}

// [ORB-11253] A live worker ceiling, observed by the admission path.

/// A running drain with checkpoint state, as the engine leaves one behind.
fn seed_running_drain(runtime: &OrbitRuntime, submitted: u32) -> String {
    seed_running_drain_input(runtime, json!({ "max_active_leaf_runs": submitted }))
}

fn seed_running_drain_input(runtime: &OrbitRuntime, input: Value) -> String {
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

fn set_worker_limit(runtime: &OrbitRuntime, run_id: &str, concurrency: u32) {
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

fn seed_backlog_leaves(runtime: &OrbitRuntime, count: usize) -> Vec<String> {
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

#[test]
fn a_stopped_drain_admits_no_leaves_or_epics_and_leaves_live_children() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    seed_backlog_leaves(&runtime, 1);
    let epic = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Epic root".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            tags: vec!["epic".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed epic");
    let drain_run_id = seed_running_drain(&runtime, 5);
    let live = seed_live_leaf_run(&runtime, &["CARRIED"]);

    runtime
        .stop_workspace_auto_admissions(crate::application::job::DrainAdmissionsStopRequest {
            actor: "tester",
            source: "unit",
            reason: None,
            claim_token: None,
        })
        .expect("stop drain");

    let output = classify_with(
        &runtime,
        json!({ "run_id": drain_run_id, "max_active_leaf_runs": 5 }),
    );
    assert_eq!(output["admissions_stopped"], true);
    assert_eq!(output["free_slots"], 0);
    assert!(
        output["loose_task_ids"]
            .as_array()
            .expect("admitted")
            .is_empty(),
        "a stopped drain admits no leaves: {output}"
    );
    assert_eq!(output["epic_task_id"], Value::Null);
    assert_eq!(output["has_epic"], false);
    assert_eq!(output["has_leaves"], false);
    let child = runtime.show_job_run(&live).expect("show child");
    assert!(
        !child.state.is_terminal(),
        "stop must not cancel an already admitted child"
    );
    assert_ne!(
        output["epic_task_id"],
        json!(epic.id),
        "a stopped drain must not start an admissible epic"
    );

    let readiness = readiness(&runtime, &[], None);
    assert_eq!(readiness["capacity"]["admissions_stopped"], true);
    assert_eq!(readiness["capacity"]["free_slots"], 0);
}

#[test]
fn a_raised_ceiling_is_observed_by_the_next_admission_pass() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    for index in 0..7 {
        write_workspace_file(&repo_root, &format!("crates/leaf_{index}/src/lib.rs"));
    }
    seed_backlog_leaves(&runtime, 7);
    let drain_run_id = seed_running_drain(&runtime, 5);
    let input = json!({ "run_id": drain_run_id, "max_active_leaf_runs": 5 });

    let before = classify_with(&runtime, input.clone());
    assert_eq!(before["max_active_leaf_runs"], 5);
    assert_eq!(before["worker_limit_source"], "run_input");
    assert_eq!(before["free_slots"], 5);
    assert_eq!(
        before["loose_task_ids"].as_array().expect("admitted").len(),
        5
    );

    set_worker_limit(&runtime, &drain_run_id, 7);

    let after = classify_with(&runtime, input);
    assert_eq!(after["max_active_leaf_runs"], 7);
    assert_eq!(after["submitted_max_active_leaf_runs"], 5);
    assert_eq!(after["worker_limit_source"], "run_control");
    assert_eq!(after["worker_limit"]["previous_max_active_leaf_runs"], 5);
    assert_eq!(after["worker_limit"]["revision"], 1);
    assert_eq!(after["free_slots"], 7);
    assert_eq!(
        after["loose_task_ids"].as_array().expect("admitted").len(),
        7
    );
}

#[test]
fn a_lowered_ceiling_stops_admissions_without_touching_live_children() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    for index in 0..3 {
        write_workspace_file(&repo_root, &format!("crates/leaf_{index}/src/lib.rs"));
    }
    let backlog = seed_backlog_leaves(&runtime, 3);
    let drain_run_id = seed_running_drain(&runtime, 5);
    // Four children are already in flight when the ceiling drops to two.
    let live: Vec<String> = (0..4)
        .map(|index| seed_live_leaf_run(&runtime, &[&format!("CARRIED-{index}")]))
        .collect();

    set_worker_limit(&runtime, &drain_run_id, 2);
    let output = classify_with(
        &runtime,
        json!({ "run_id": drain_run_id, "max_active_leaf_runs": 5 }),
    );

    assert_eq!(output["max_active_leaf_runs"], 2);
    assert_eq!(output["active_leaf_runs"], 4);
    assert_eq!(output["free_slots"], 0);
    assert!(
        output["loose_task_ids"]
            .as_array()
            .expect("admitted")
            .is_empty(),
        "an over-capacity drain admits nothing: {output}"
    );
    assert_eq!(output["pending_backlog"], backlog.len());
    // Nothing was cancelled: every child is still a live leaf run.
    for run_id in &live {
        let child = runtime.show_job_run(run_id).expect("show child run");
        assert!(
            !child.state.is_terminal(),
            "child {run_id} was terminalized"
        );
    }
}

#[test]
fn a_drain_that_cannot_be_identified_keeps_its_submitted_ceiling() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    seed_backlog_leaves(&runtime, 1);

    // No `run_id` (a direct dispatch) and an unknown one both degrade to the
    // submitted ceiling rather than failing the iteration.
    for input in [
        json!({ "max_active_leaf_runs": 3 }),
        json!({ "run_id": "jrun-missing", "max_active_leaf_runs": 3 }),
    ] {
        let output = classify_with(&runtime, input);
        assert_eq!(output["max_active_leaf_runs"], 3);
        assert_eq!(output["worker_limit_source"], "run_input");
        assert_eq!(output["worker_limit"], Value::Null);
    }
}

#[test]
fn readiness_reports_the_live_ceiling_and_who_moved_it() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    let backlog = seed_backlog_leaves(&runtime, 1);
    let drain_run_id = seed_running_drain(&runtime, 5);

    let submitted = readiness(&runtime, &backlog, None);
    assert_eq!(submitted["capacity"]["max_active_leaf_runs"], 5);
    assert_eq!(submitted["capacity"]["limit_source"], "run_input");
    assert_eq!(submitted["capacity"]["drain_run_id"], drain_run_id);

    set_worker_limit(&runtime, &drain_run_id, 7);

    let adjusted = readiness(&runtime, &backlog, None);
    assert_eq!(adjusted["capacity"]["max_active_leaf_runs"], 7);
    assert_eq!(adjusted["capacity"]["limit_source"], "run_control");
    assert_eq!(adjusted["capacity"]["worker_limit"]["actor"], "tester");
    assert_eq!(adjusted["capacity"]["worker_limit"]["revision"], 1);

    // An explicit `--concurrency` still previews what the operator typed.
    let previewed = readiness(&runtime, &backlog, Some(2));
    assert_eq!(previewed["capacity"]["max_active_leaf_runs"], 2);
    assert_eq!(previewed["capacity"]["limit_source"], "requested");
}

/// [ORB-11273] `orbit run job ... --input max_active_leaf_runs=7` persists the
/// ceiling as a JSON string. Readiness must report that live drain ceiling
/// (and the same source the classifier uses), not the numeric-only fallback of 5.
#[test]
fn readiness_parses_numeric_and_string_run_input_ceilings() {
    for submitted in [json!(7), json!("7")] {
        let (_root, runtime, repo_root) = runtime_with_workspace_layout();
        write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
        let backlog = seed_backlog_leaves(&runtime, 1);
        let drain_run_id =
            seed_running_drain_input(&runtime, json!({ "max_active_leaf_runs": submitted }));

        let output = readiness(&runtime, &backlog, None);
        assert_eq!(
            output["capacity"]["max_active_leaf_runs"], 7,
            "submitted {submitted} must report the live ceiling, not the default 5"
        );
        assert_eq!(output["capacity"]["limit_source"], "run_input");
        assert_eq!(output["capacity"]["drain_run_id"], drain_run_id);

        let classified = classify_with(
            &runtime,
            json!({ "run_id": drain_run_id, "max_active_leaf_runs": submitted }),
        );
        assert_eq!(classified["max_active_leaf_runs"], 7);
        assert_eq!(classified["submitted_max_active_leaf_runs"], 7);
        assert_eq!(classified["worker_limit_source"], "run_input");
    }
}

#[test]
fn complexity_pools_drive_allowlist_eligibility_without_reassigning_manual_tasks() {
    let (_root, runtime, _repo) = runtime_with_workspace_config(Some(
        "[workflow]\ndefault_crew = \"opus\"\nmedium_complexity_crews = [\"grok\", \"terra\"]\n",
    ));
    let unassigned = runtime
        .add_task(TaskAddParams {
            title: "Pool-eligible task".into(),
            description: "Configured pool permits this task despite its excluded default".into(),
            plan: "Inspect classification".into(),
            complexity: orbit_types::task::TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("unassigned task");
    let manual = runtime
        .add_task(TaskAddParams {
            title: "Manual crew outside automatic pool".into(),
            description: "Manual assignments remain selectable".into(),
            plan: "Inspect classification".into(),
            complexity: orbit_types::task::TaskComplexity::Medium,
            crew: Some("astra".into()),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("manual task");
    let unrestricted = classify(&runtime);
    let ids = unrestricted["loose_task_ids"].as_array().expect("ids");
    assert!(ids.contains(&json!(unassigned.id)));
    assert!(
        ids.contains(&json!(manual.id)),
        "a pool must not install an allowlist"
    );
    let restricted = classify_with(&runtime, json!({"allowed_crews": ["terra"]}));
    assert_eq!(restricted["loose_task_ids"], json!([unassigned.id]));
    let disjoint = classify_with(&runtime, json!({"allowed_crews": ["luna"]}));
    assert_eq!(disjoint["loose_task_ids"], json!([]));
    let readiness = runtime
        .workspace_auto_readiness(
            std::slice::from_ref(&unassigned.id),
            None,
            10,
            &["luna".into()],
        )
        .expect("readiness");
    let excluded = readiness_task(&readiness, &unassigned.id);
    assert_eq!(excluded["reason"], "crew_not_allowed");
    assert_eq!(excluded["crew"], "grok, terra");
    assert_eq!(
        runtime
            .get_task(&manual.id)
            .expect("manual task")
            .crew
            .as_deref(),
        Some("astra")
    );
}

#[test]
fn classifier_uses_captured_cli_pool_instead_of_current_configuration() {
    let (_root, runtime, _repo) =
        runtime_with_workspace_config(Some("[workflow]\nmedium_complexity_crews = [\"grok\"]\n"));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Captured pool task".into(),
            description: "CLI override controls eligibility".into(),
            plan: "Inspect classifier against coordinator input".into(),
            complexity: orbit_types::task::TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task");
    let mut input = json!({"medium_complexity_crews": ["terra"], "allowed_crews": ["terra"]});
    runtime
        .install_auto_crew_admission(
            "workspace_auto_pipeline",
            &mut input,
            None,
            false,
            &mut || panic!("capture does not draw"),
        )
        .expect("capture");
    let coordinator = runtime
        .stores()
        .jobs()
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), Some(input), None)
        .expect("coordinator");
    let result = classify_with(
        &runtime,
        json!({"run_id": coordinator.run_id, "allowed_crews": ["terra"]}),
    );
    assert_eq!(result["loose_task_ids"], json!([task.id]));
}
