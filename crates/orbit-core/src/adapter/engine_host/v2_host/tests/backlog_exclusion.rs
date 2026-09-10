use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::backlog_exclusion::sort_tasks_for_automatic_dispatch;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, seed_list_backlog_task, write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn list_backlog_tasks(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("list backlog tasks")
}

fn excluded_entry<'a>(output: &'a Value, task_id: &str) -> &'a Value {
    output["excluded"]
        .as_array()
        .expect("excluded array")
        .iter()
        .find(|entry| entry["id"] == task_id)
        .expect("excluded entry")
}

fn output_task_ids(output: &Value) -> Vec<String> {
    output["task_ids"]
        .as_array()
        .expect("task_ids array")
        .iter()
        .map(|task_id| {
            task_id
                .as_str()
                .expect("task_id should be a string")
                .to_string()
        })
        .collect()
}

#[test]
fn list_backlog_tasks_empty_workspace_is_a_clean_noop() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output["task_count"], json!(0));
    assert_eq!(output["task_ids"], json!([]));
    assert_eq!(output["bundles"], json!([]));
    assert_eq!(output["excluded"], json!([]));
}

#[test]
fn automatic_admission_withholds_unassessed_tasks_until_task_pilot_prepares_them() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Automated repair awaiting assessment".to_string(),
            description: "Automated repair fixture".to_string(),
            acceptance_criteria: vec!["Repair is validated.".to_string()],
            plan: "Prepare with task-pilot.".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Bug),
            system_created: true,
            ..TaskAddParams::default()
        })
        .expect("seed unassessed automated task");

    let output = list_backlog_tasks(&runtime, json!({}));

    assert!(!output_task_ids(&output).contains(&task.id));
    assert_eq!(
        excluded_entry(&output, &task.id)["reason"],
        "unassessed_complexity"
    );
    let error = runtime
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({"task_ids": [task.id]}),
            ToolContext::default(),
        )
        .expect_err("explicit implementation admission must not bypass preparation");
    assert!(
        error.to_string().contains("task-pilot preparation"),
        "{error}"
    );
}

fn seed_task_with_dependencies(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    dependencies: Vec<String>,
) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["Fixture task is observable.".to_string()],
            dependencies,
            plan: "Fixture plan.".to_string(),
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..Default::default()
        })
        .expect("seed task with dependencies")
}

fn seed_backlog_task_with_dependencies(
    runtime: &OrbitRuntime,
    title: &str,
    dependencies: Vec<String>,
) -> Task {
    seed_task_with_dependencies(runtime, title, TaskStatus::Backlog, dependencies)
}

#[test]
fn list_backlog_tasks_preserves_existing_fields_without_conflicts() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/alpha/src/lib.rs");
    write_workspace_file(&repo_root, "crates/beta/src/lib.rs");
    let medium = seed_list_backlog_task(
        &runtime,
        "Medium backlog",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/alpha/src/lib.rs"],
    );
    let high = seed_list_backlog_task(
        &runtime,
        "High backlog",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec!["crates/beta/src/lib.rs"],
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output["task_count"], json!(2));
    assert_eq!(output["task_ids"], json!([high.id, medium.id]));
    assert_eq!(
        output["tasks"],
        json!([
            {
                "id": high.id,
                "title": "High backlog",
                "type": "chore",
                "priority": "high",
                "context_files": high.context_files,
                "parent_id": null
            },
            {
                "id": medium.id,
                "title": "Medium backlog",
                "type": "chore",
                "priority": "medium",
                "context_files": medium.context_files,
                "parent_id": null
            }
        ])
    );
    assert_eq!(output["excluded"], json!([]));
}

#[test]
fn automatic_backlog_selection_prioritizes_critical_then_corrective_work() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let feature = seed_list_backlog_task(
        &runtime,
        "High feature",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Feature,
        None,
        vec![],
    );
    let bug = seed_list_backlog_task(
        &runtime,
        "Low bug",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Bug,
        None,
        vec![],
    );
    let critical = seed_list_backlog_task(
        &runtime,
        "Critical feature",
        TaskStatus::Backlog,
        TaskPriority::Critical,
        TaskType::Feature,
        None,
        vec![],
    );

    let output = list_backlog_tasks(&runtime, json!({ "max_tasks": 1 }));
    assert_eq!(output["task_ids"], json!([critical.id]));

    runtime
        .update_task(
            &critical.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("complete critical task");
    let output = list_backlog_tasks(&runtime, json!({ "max_tasks": 1 }));
    assert_eq!(output["task_ids"], json!([bug.id]));
    assert_ne!(output["task_ids"], json!([feature.id]));
}

#[test]
fn only_exact_review_tags_enter_the_corrective_band() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let title_only = runtime
        .add_task(TaskAddParams {
            title: "[code-review] Title provenance is not structured".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Selected deterministically".to_string()],
            plan: "Fixture plan".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed title-only task");
    let partial_tag = runtime
        .add_task(TaskAddParams {
            title: "Review sweep".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Selected deterministically".to_string()],
            tags: vec!["code-review-sweep".to_string()],
            plan: "Fixture plan".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed partial-tag task");
    let unrelated_tag = runtime
        .add_task(TaskAddParams {
            title: "QA follow-up".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Selected deterministically".to_string()],
            tags: vec!["review".to_string()],
            plan: "Fixture plan".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed unrelated-tag task");
    let code_review = runtime
        .add_task(TaskAddParams {
            title: "Confirmed code review finding".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Selected deterministically".to_string()],
            tags: vec!["code-review".to_string()],
            plan: "Fixture plan".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed code-review task");
    let security_review = runtime
        .add_task(TaskAddParams {
            title: "Confirmed security review finding".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Selected deterministically".to_string()],
            tags: vec!["security-review".to_string()],
            plan: "Fixture plan".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed security-review task");

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(
        output_task_ids(&output),
        vec![
            code_review.id,
            security_review.id,
            title_only.id,
            partial_tag.id,
            unrelated_tag.id,
        ]
    );
}

#[test]
fn automatic_dispatch_uses_task_id_as_the_total_tie_breaker() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut task_two = seed_list_backlog_task(
        &runtime,
        "Second ID",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let mut task_one = task_two.clone();
    task_two.id = "ORB-00002".to_string();
    task_one.id = "ORB-00001".to_string();
    let mut tasks = vec![task_two, task_one];

    sort_tasks_for_automatic_dispatch(&mut tasks);

    assert_eq!(
        tasks.into_iter().map(|task| task.id).collect::<Vec<_>>(),
        vec!["ORB-00001", "ORB-00002"]
    );
}

#[test]
fn list_backlog_tasks_does_not_filter_auto_task_provenance_tags() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let ordinary = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Ordinary backlog".to_string(),
            description: "ordinary".to_string(),
            acceptance_criteria: vec!["selected".to_string()],
            plan: "ship".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed ordinary backlog");
    let auto_task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Auto-task backlog".to_string(),
            description: "minted by scheduler".to_string(),
            acceptance_criteria: vec!["selected".to_string()],
            tags: vec!["auto-task:nightly-maintenance".to_string()],
            plan: "ship".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            ..Default::default()
        })
        .expect("seed auto-task backlog");

    let output = list_backlog_tasks(&runtime, json!({}));
    let selected = output_task_ids(&output);

    assert_eq!(selected.len(), 2);
    assert!(selected.contains(&ordinary.id));
    assert!(selected.contains(&auto_task.id));
}

#[test]
fn list_backlog_tasks_filters_dependency_readiness() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let proposed_dependency = seed_task_with_dependencies(
        &runtime,
        "Proposed dependency",
        TaskStatus::Proposed,
        vec![],
    );
    let backlog_dependency =
        seed_task_with_dependencies(&runtime, "Backlog dependency", TaskStatus::Backlog, vec![]);
    let in_progress_dependency = seed_task_with_dependencies(
        &runtime,
        "In-progress dependency",
        TaskStatus::InProgress,
        vec![],
    );
    let review_dependency =
        seed_task_with_dependencies(&runtime, "Review dependency", TaskStatus::Review, vec![]);
    let done_dependency =
        seed_task_with_dependencies(&runtime, "Done dependency", TaskStatus::Done, vec![]);
    let ready = seed_backlog_task_with_dependencies(
        &runtime,
        "Ready dependent",
        vec![done_dependency.id.clone()],
    );
    let no_dependencies = seed_backlog_task_with_dependencies(&runtime, "No dependencies", vec![]);
    let blocked_by_proposed = seed_backlog_task_with_dependencies(
        &runtime,
        "Blocked by proposed",
        vec![proposed_dependency.id.clone()],
    );
    let blocked_by_backlog = seed_backlog_task_with_dependencies(
        &runtime,
        "Blocked by backlog",
        vec![backlog_dependency.id.clone()],
    );
    let blocked_by_in_progress = seed_backlog_task_with_dependencies(
        &runtime,
        "Blocked by in-progress",
        vec![in_progress_dependency.id.clone()],
    );
    let blocked_by_review = seed_backlog_task_with_dependencies(
        &runtime,
        "Blocked by review",
        vec![review_dependency.id.clone()],
    );

    let output = list_backlog_tasks(&runtime, json!({}));
    let task_ids = output_task_ids(&output);

    assert!(task_ids.contains(&ready.id));
    assert!(task_ids.contains(&no_dependencies.id));
    assert!(task_ids.contains(&backlog_dependency.id));
    assert!(!task_ids.contains(&blocked_by_proposed.id));
    assert!(!task_ids.contains(&blocked_by_backlog.id));
    assert!(!task_ids.contains(&blocked_by_in_progress.id));
    assert!(!task_ids.contains(&blocked_by_review.id));
    assert_eq!(output["excluded"], json!([]));
}

#[test]
fn list_backlog_tasks_serializes_orb_00042_grok_epic_chain() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let orb43 = seed_backlog_task_with_dependencies(&runtime, "ORB-00043 grok foundation", vec![]);
    let orb44 = seed_backlog_task_with_dependencies(
        &runtime,
        "ORB-00044 grok follow-up",
        vec![orb43.id.clone()],
    );
    let orb45 = seed_backlog_task_with_dependencies(
        &runtime,
        "ORB-00045 grok follow-up",
        vec![orb43.id.clone()],
    );
    let orb46 = seed_backlog_task_with_dependencies(
        &runtime,
        "ORB-00046 grok follow-up",
        vec![orb43.id.clone()],
    );
    let orb48 = seed_backlog_task_with_dependencies(
        &runtime,
        "ORB-00048 grok follow-up",
        vec![orb43.id.clone()],
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output_task_ids(&output), vec![orb43.id.clone()]);

    runtime
        .update_task(
            &orb43.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )
        .expect("mark ORB-00043 done");
    let output = list_backlog_tasks(&runtime, json!({}));
    let task_ids = output_task_ids(&output);

    assert_eq!(task_ids.len(), 4);
    assert!(task_ids.contains(&orb44.id));
    assert!(task_ids.contains(&orb45.id));
    assert!(task_ids.contains(&orb46.id));
    assert!(task_ids.contains(&orb48.id));
}

#[test]
fn list_backlog_tasks_reports_direct_context_lock_conflicts() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/foo/src/lib.rs");
    let locking = seed_list_backlog_task(
        &runtime,
        "Locking task",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/foo/src/lib.rs"],
    );
    let backlog = seed_list_backlog_task(
        &runtime,
        "Backlog task",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/foo/src/lib.rs"],
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output["task_count"], json!(0));
    assert_eq!(output["task_ids"], json!([]));
    assert_eq!(output["tasks"], json!([]));
    assert_eq!(output["bundles"], json!([]));
    assert_eq!(
        output["excluded"],
        json!([{
            "id": backlog.id,
            "reason": "context_lock_conflict",
            "conflicts": [{
                "requested_file": backlog.context_files[0],
                "locking_task_id": locking.id
            }]
        }])
    );
}

#[test]
fn active_epic_excludes_only_loose_tasks_overlapping_descendant_union() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/epic/src/lib.rs");
    write_workspace_file(&repo_root, "crates/loose/src/lib.rs");
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Active epic".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["Assembled".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Drain children".to_string(),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed active epic");
    seed_list_backlog_task(
        &runtime,
        "Epic child",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        Some(epic.id.clone()),
        vec!["file:crates/epic/src/lib.rs"],
    );
    let overlapping = seed_list_backlog_task(
        &runtime,
        "Overlapping loose leaf",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec!["file:crates/epic/src/lib.rs"],
    );
    let unrelated = seed_list_backlog_task(
        &runtime,
        "Unrelated loose leaf",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["file:crates/loose/src/lib.rs"],
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output_task_ids(&output), vec![unrelated.id]);
    let excluded = excluded_entry(&output, &overlapping.id);
    assert_eq!(excluded["reason"], "context_lock_conflict");
    assert_eq!(excluded["conflicts"][0]["locking_task_id"], epic.id);
}

#[test]
fn list_backlog_tasks_reports_group_member_conflicts_with_trigger_conflicts() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "docs/parent.md");
    write_workspace_file(&repo_root, "crates/foo/src/lib.rs");
    write_workspace_file(&repo_root, "crates/bar/src/lib.rs");
    let foo_lock = seed_list_backlog_task(
        &runtime,
        "Foo lock",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/foo/src/lib.rs"],
    );
    let bar_lock = seed_list_backlog_task(
        &runtime,
        "Bar lock",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/bar/src/lib.rs"],
    );
    let parent = seed_list_backlog_task(
        &runtime,
        "Parent",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["docs/parent.md"],
    );
    let low_child = seed_list_backlog_task(
        &runtime,
        "Low child",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(parent.id.clone()),
        vec!["crates/foo/src/lib.rs"],
    );
    let high_child = seed_list_backlog_task(
        &runtime,
        "High child",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        Some(parent.id.clone()),
        vec!["crates/bar/src/lib.rs"],
    );

    let output = list_backlog_tasks(&runtime, json!({}));

    assert_eq!(output["task_count"], json!(0));
    assert_eq!(output["excluded"].as_array().expect("excluded").len(), 3);
    assert_eq!(
        excluded_entry(&output, &parent.id),
        &json!({
            "id": parent.id,
            "reason": "group_member_conflict",
            "conflicts": [{
                "requested_file": high_child.context_files[0],
                "locking_task_id": bar_lock.id
            }]
        })
    );
    assert_eq!(
        excluded_entry(&output, &high_child.id),
        &json!({
            "id": high_child.id,
            "reason": "context_lock_conflict",
            "conflicts": [{
                "requested_file": high_child.context_files[0],
                "locking_task_id": bar_lock.id
            }]
        })
    );
    assert_eq!(
        excluded_entry(&output, &low_child.id),
        &json!({
            "id": low_child.id,
            "reason": "context_lock_conflict",
            "conflicts": [{
                "requested_file": low_child.context_files[0],
                "locking_task_id": foo_lock.id
            }]
        })
    );
}

#[test]
fn list_backlog_tasks_does_not_report_max_tasks_truncation_as_excluded() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    for index in 0..3 {
        let path = format!("docs/task-{index}.md");
        write_workspace_file(&repo_root, &path);
        seed_list_backlog_task(
            &runtime,
            &format!("Task {index}"),
            TaskStatus::Backlog,
            TaskPriority::Medium,
            TaskType::Chore,
            None,
            vec![&path],
        );
    }

    let output = list_backlog_tasks(&runtime, json!({ "max_tasks": 2 }));

    assert_eq!(output["task_count"], json!(2));
    assert_eq!(output["task_ids"].as_array().expect("task_ids").len(), 2);
    assert_eq!(output["excluded"], json!([]));
}

#[test]
fn list_backlog_tasks_omits_excluded_for_explicit_task_ids() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/foo/src/lib.rs");
    seed_list_backlog_task(
        &runtime,
        "Locking task",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/foo/src/lib.rs"],
    );
    let backlog = seed_list_backlog_task(
        &runtime,
        "Backlog task",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/foo/src/lib.rs"],
    );
    let corrective = seed_list_backlog_task(
        &runtime,
        "Corrective task",
        TaskStatus::Backlog,
        TaskPriority::Low,
        TaskType::Bug,
        None,
        vec![],
    );

    let output = list_backlog_tasks(&runtime, json!({ "task_ids": [backlog.id, corrective.id] }));

    assert_eq!(output["task_count"], json!(2));
    assert_eq!(output["task_ids"], json!([backlog.id, corrective.id]));
    assert!(output.get("excluded").is_none());
}

#[test]
fn list_backlog_tasks_excludes_epic_roots_and_descendants_with_reasons() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let loose_one = seed_list_backlog_task(
        &runtime,
        "Loose one",
        TaskStatus::Backlog,
        TaskPriority::High,
        TaskType::Chore,
        None,
        vec![],
    );
    let loose_two = seed_list_backlog_task(
        &runtime,
        "Loose two",
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
            complexity: TaskComplexity::Medium,
            ..Default::default()
        })
        .expect("seed epic root");
    let children = (0..3)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Epic child {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                Some(epic.id.clone()),
                vec![],
            )
        })
        .collect::<Vec<_>>();

    let output = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(output_task_ids(&output), vec![loose_one.id, loose_two.id]);
    assert_eq!(excluded_entry(&output, &epic.id)["reason"], "epic_root");
    assert_eq!(excluded_entry(&output, &epic.id)["conflicts"], json!([]));
    for child in children {
        assert_eq!(excluded_entry(&output, &child.id)["reason"], "epic_child");
        assert_eq!(excluded_entry(&output, &child.id)["conflicts"], json!([]));
    }
}

/// `list_backlog_tasks` builds its candidate list by walking the task lookup,
/// which iterates in task-ID order rather than the store's created-at order.
/// That is only safe because the dispatch sort is a total order, so pin it:
/// two permutations of the same population must sort to the same sequence.
#[test]
fn dispatch_order_is_independent_of_the_input_order() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let mut tasks = vec![
        seed_task_with_dependencies(&runtime, "Chore alpha", TaskStatus::Backlog, Vec::new()),
        seed_task_with_dependencies(&runtime, "Chore beta", TaskStatus::Backlog, Vec::new()),
        seed_task_with_dependencies(&runtime, "Chore gamma", TaskStatus::Backlog, Vec::new()),
        seed_task_with_dependencies(&runtime, "Chore delta", TaskStatus::Backlog, Vec::new()),
    ];
    // Vary the bands so the sort has real work to do rather than falling
    // straight through to the ID tie-breaker.
    tasks[0].priority = TaskPriority::Critical;
    tasks[1].task_type = TaskType::Bug;
    tasks[2].tags.push("code-review".to_string());
    tasks[3].priority = TaskPriority::Low;

    let mut forward = tasks.clone();
    let mut reversed = tasks;
    reversed.reverse();

    sort_tasks_for_automatic_dispatch(&mut forward);
    sort_tasks_for_automatic_dispatch(&mut reversed);

    let ids = |tasks: &[Task]| tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
    assert_eq!(
        ids(&forward),
        ids(&reversed),
        "dispatch order must not depend on how the population was enumerated"
    );
    // The critical task still leads, so the assertion above is comparing a
    // meaningful ordering rather than two identically-sorted no-ops.
    assert_eq!(forward[0].priority, TaskPriority::Critical);
}
