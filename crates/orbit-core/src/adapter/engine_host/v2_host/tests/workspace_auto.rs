use std::collections::BTreeSet;

use chrono::{SecondsFormat, TimeZone, Utc};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, JobRunState};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::task_pilot::prepare;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_config, runtime_with_workspace_layout, seed_list_backlog_task,
    write_workspace_file,
};
use crate::application::auto_tasks::AutoTaskAddParams;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::runtime::host_signal::{FixedHostSignals, ScheduledShutdown};

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

#[test]
fn minted_no_diff_auto_task_is_admitted_unassessed_and_never_piloted() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: "qa-review".to_string(),
            description: "QA review".to_string(),
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
            template: AutoTaskTemplate {
                title: "QA review".to_string(),
                description: "Review the delivered change.".to_string(),
                acceptance_criteria: vec!["Review result is recorded.".to_string()],
                task_type: TaskType::Chore,
                tags: vec!["no-diff-expected".to_string()],
                required_tools: vec![],
                priority: TaskPriority::Medium,
                complexity: None,
                crew: Some("opus".to_string()),
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .expect("add auto-task definition");
    let minted = runtime.auto_task_mint("qa-review").expect("mint auto-task");

    assert_eq!(minted.complexity, Some(TaskComplexity::Unassessed));
    assert_eq!(minted.crew.as_deref(), Some("opus"));
    assert!(minted.tags.iter().any(|tag| tag == "no-diff-expected"));
    assert!(minted.tags.iter().any(|tag| tag == "auto-task:qa-review"));

    // The `no-diff-expected` tag exempts the task from the assessed-complexity
    // gate, so it is admissible before task-pilot ever sees it [ORB-12118].
    let before = classify(&runtime);
    assert!(
        before["loose_task_ids"]
            .as_array()
            .expect("admitted tasks")
            .contains(&json!(minted.id))
    );

    // Automatic discovery never assesses no-diff work: its result lives
    // outside the repository, so there are no modification selectors to pick,
    // and admitting it re-piloted the same task on every routine tick.
    let prepared = prepare(
        &runtime,
        "prepare_task_pilot",
        &json!({ "workspace_path": repo_root }),
    )
    .expect("automatic preparation runs without the minted no-diff auto-task");
    assert!(
        !prepared["task_ids"]
            .as_array()
            .expect("task ids")
            .contains(&json!(minted.id))
    );
    assert!(
        prepared["excluded"]
            .as_array()
            .expect("excluded")
            .iter()
            .any(|entry| entry["task_id"] == minted.id && entry["reason"] == "no_diff_task")
    );

    let untouched = runtime.get_task(&minted.id).expect("minted task");
    assert_eq!(untouched.complexity, Some(TaskComplexity::Unassessed));
    assert!(untouched.context_files.is_empty());
    assert_eq!(untouched.crew.as_deref(), Some("opus"));

    let after = classify(&runtime);
    assert!(
        after["loose_task_ids"]
            .as_array()
            .expect("admitted tasks")
            .contains(&json!(minted.id))
    );
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
fn readiness_explains_dependencies_locks_children_claims_and_capacity() {
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
    // [ORB-12491] A child of an `epic`-tagged root is an ordinary leaf: it is
    // never "managed", and only the scarce slot keeps it waiting.
    let tagged_root = runtime
        .add_task(TaskAddParams {
            title: "Tagged root".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            tags: vec!["epic".to_string()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed tagged root");
    let child = seed_list_backlog_task(
        &runtime,
        "Child of a tagged root",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(tagged_root.id),
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
        child.id.clone(),
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
        readiness_task(&output, &child.id)["reason"],
        "capacity_saturated"
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
fn readiness_uses_the_classifier_candidate_pool() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/shared/src/lib.rs");
    write_workspace_file(&repo_root, "crates/outside/src/lib.rs");

    let mut task_ids = (0..50)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Conflicting candidate {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec!["crates/shared/src/lib.rs"],
            )
            .id
        })
        .collect::<Vec<_>>();
    let outside_pool = seed_list_backlog_task(
        &runtime,
        "Conflict-free task outside the candidate pool",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/outside/src/lib.rs"],
    );
    task_ids.push(outside_pool.id.clone());

    let classified = classify_with(&runtime, json!({ "max_active_leaf_runs": 4 }));
    let readiness = runtime
        .workspace_auto_readiness(&task_ids, Some(4), 51, &[])
        .expect("explain readiness");
    let readiness_admitted = readiness["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .filter(|task| task["eligible"] == true)
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    let classified_admitted = classified["loose_task_ids"]
        .as_array()
        .expect("classified task ids");
    assert_eq!(&readiness_admitted, classified_admitted);
    assert_eq!(readiness["capacity"]["candidate_pool_size"], 50);
    assert_eq!(readiness["capacity"]["candidate_pool_truncated"], true);
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["reason"],
        "outside_candidate_pool"
    );
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["eligible"],
        false
    );
}

#[test]
fn readiness_uses_the_active_drain_candidate_pool_for_a_conflict_free_tail() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/shared/src/lib.rs");
    write_workspace_file(&repo_root, "crates/outside/src/lib.rs");

    let mut task_ids = (0..50)
        .map(|index| {
            seed_list_backlog_task(
                &runtime,
                &format!("Conflicting candidate {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                None,
                vec!["crates/shared/src/lib.rs"],
            )
            .id
        })
        .collect::<Vec<_>>();
    let outside_pool = seed_list_backlog_task(
        &runtime,
        "Conflict-free task after the candidate pool",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec!["crates/outside/src/lib.rs"],
    );
    task_ids.push(outside_pool.id.clone());

    let drain_run_id = seed_running_drain_input(
        &runtime,
        json!({ "max_active_leaf_runs": 4, "max_tasks": "51" }),
    );
    let classified = classify_with(
        &runtime,
        json!({
            "run_id": drain_run_id,
            "max_active_leaf_runs": 4,
            "max_tasks": "51",
        }),
    );
    let readiness = runtime
        .workspace_auto_readiness(&task_ids, None, 51, &[])
        .expect("explain readiness");
    let readiness_admitted = readiness["tasks"]
        .as_array()
        .expect("readiness tasks")
        .iter()
        .filter(|task| task["eligible"] == true)
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    assert_eq!(
        &readiness_admitted,
        classified["loose_task_ids"]
            .as_array()
            .expect("classified task ids")
    );
    assert_eq!(readiness["capacity"]["candidate_pool_size"], 51);
    assert_eq!(readiness["capacity"]["candidate_pool_truncated"], false);
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["reason"],
        "ready"
    );
    assert_eq!(
        readiness_task(&readiness, &outside_pool.id)["eligible"],
        true
    );
}

#[test]
fn readiness_candidate_pool_parsing_matches_classifier_for_numeric_and_string_one() {
    for max_tasks in [json!(1), json!("1")] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let tasks = seed_backlog_leaves(&runtime, 2);
        let drain_run_id = seed_running_drain_input(
            &runtime,
            json!({ "max_active_leaf_runs": 2, "max_tasks": max_tasks }),
        );
        let input = json!({
            "run_id": drain_run_id,
            "max_active_leaf_runs": 2,
            "max_tasks": max_tasks,
        });

        let classified = classify_with(&runtime, input);
        let readiness = runtime
            .workspace_auto_readiness(&tasks, None, tasks.len(), &[])
            .expect("explain readiness");

        assert_eq!(classified["candidate_pool_size"], 1);
        assert_eq!(readiness["capacity"]["candidate_pool_size"], 1);
        assert_eq!(classified["loose_task_ids"], json!([tasks[0]]));
        assert_eq!(readiness_task(&readiness, &tasks[0])["reason"], "ready");
        assert_eq!(
            readiness_task(&readiness, &tasks[1])["reason"],
            "outside_candidate_pool"
        );
    }
}

#[test]
fn an_epic_tagged_root_and_its_children_are_admitted_as_ordinary_leaves() {
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
    // [ORB-12491] The tag is a size hint. The root takes a slot like any other
    // leaf, in its own priority/age position, and its children are admitted
    // alongside it rather than withheld for it.
    let tagged_root = runtime
        .add_task(TaskAddParams {
            title: "Tagged root".to_string(),
            description: "Root fixture".to_string(),
            acceptance_criteria: vec!["Delivered".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Do the large task".to_string(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Hard,
            ..Default::default()
        })
        .expect("seed tagged root");
    let mut children = Vec::new();
    for index in 0..3 {
        children.push(
            seed_list_backlog_task(
                &runtime,
                &format!("Child {index}"),
                TaskStatus::Backlog,
                TaskPriority::Medium,
                TaskType::Chore,
                Some(tagged_root.id.clone()),
                vec![],
            )
            .id,
        );
    }

    // A ceiling above the population, so what the wave omits is a decision and
    // not a free-slot artifact.
    let admissible = classify_with(&runtime, json!({ "max_active_leaf_runs": 10 }));
    let admitted = admissible["loose_task_ids"]
        .as_array()
        .expect("loose task ids")
        .iter()
        .map(|task_id| task_id.as_str().expect("task id").to_string())
        .collect::<BTreeSet<_>>();
    assert!(admitted.contains(&loose_one.id), "{admitted:?}");
    assert!(admitted.contains(&loose_two.id), "{admitted:?}");
    assert!(admitted.contains(&tagged_root.id), "{admitted:?}");
    for child in &children {
        assert!(admitted.contains(child), "{admitted:?}");
    }
    assert_eq!(admissible["has_leaves"], true);
    assert_eq!(admissible["idle"], false);
    assert!(admissible.get("epic_task_id").is_none());
    assert!(admissible.get("has_epic").is_none());
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
model = "gpt-6-sol"

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
            complexity: TaskComplexity::Medium,
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
            complexity: TaskComplexity::Medium,
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
            complexity: TaskComplexity::Medium,
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
/// long as a large root was `in-progress`. Admission is that task's own lock
/// reservation instead: the leaf that overlaps its declared files is excluded,
/// and the one that does not still ships in the same drain [ORB-12491].
#[test]
fn a_live_tagged_root_excludes_only_the_leaves_that_overlap_its_own_files() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/epic/src/lib.rs");
    write_workspace_file(&repo_root, "crates/elsewhere/src/lib.rs");
    let tagged_root = runtime
        .add_task(TaskAddParams {
            title: "Active tagged root".to_string(),
            description: "Root fixture".to_string(),
            acceptance_criteria: vec!["Delivered".to_string()],
            tags: vec!["epic".to_string()],
            plan: "Do the large task".to_string(),
            context_files: vec!["file:crates/epic/src/lib.rs".to_string()],
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("seed active tagged root");
    // [ORB-12491] The root holds its own declared file, not its child's: the
    // child's surface is the child's to reserve when it starts.
    let child = seed_list_backlog_task(
        &runtime,
        "Child elsewhere",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Chore,
        Some(tagged_root.id.clone()),
        vec!["crates/child/src/lib.rs"],
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
    assert_eq!(
        admissible["loose_task_ids"],
        json!([child.id, conflict_free.id])
    );
    assert_eq!(admissible["has_leaves"], true);
    assert_eq!(admissible["idle"], false);
    assert!(
        !admissible["loose_task_ids"]
            .as_array()
            .expect("loose task ids")
            .contains(&json!(overlapping.id)),
        "a leaf overlapping the root's declared files must not ship"
    );
}

#[test]
fn an_empty_workspace_is_admissibly_empty() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();

    let quiet = classify(&runtime);

    assert_eq!(quiet["loose_task_ids"], json!([]));
    assert_eq!(quiet["has_leaves"], false);
    assert_eq!(quiet["idle"], true);
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

/// [ORB-12617] The legacy drain allocates against the *shared* ceiling.
///
/// A pulled claim binds a `task_claimed_*_pipeline` run with no wrapper above
/// it, and an admission that has been requested but never reached a run holds
/// capacity nothing else can see. Counting only `task_auto_pipeline` runs here
/// would let the two admission paths each fill the whole ceiling, so the
/// classifier reads the same occupancy the pull allocator commits against.
#[test]
fn claimed_leaves_and_pending_admissions_consume_the_legacy_drain_ceiling() {
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
    let jobs = runtime.stores().jobs();

    // A claimed leaf nothing dispatched: no wrapper, but a real occupant.
    jobs.insert_job_run("task_claimed_local_pipeline", 1, Utc::now(), None, None)
        .expect("claimed leaf");
    let mixed = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
    assert_eq!(mixed["active_leaf_runs"], 1);
    assert_eq!(
        mixed["wrapper_leaf_runs"], 0,
        "the occupant is not a wrapper, and the breakdown says so"
    );
    assert_eq!(
        mixed["leaf_occupancy_by_pipeline"]["task_claimed_local_pipeline"],
        1
    );
    assert_eq!(mixed["free_slots"], 2);
    assert_eq!(
        mixed["loose_task_ids"],
        json!([seeded[0].id, seeded[1].id]),
        "a claimed leaf costs the legacy drain a slot"
    );

    // A live wrapper takes the second slot; readiness reads the same numbers.
    seed_live_leaf_run(&runtime, &[seeded[0].id.as_str()]);
    let both = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
    assert_eq!(both["active_leaf_runs"], 2);
    assert_eq!(both["wrapper_leaf_runs"], 1);
    assert_eq!(both["free_slots"], 1);
    let explained = readiness(&runtime, &[], Some(3));
    assert_eq!(explained["capacity"]["active_leaf_runs"], 2);
    assert_eq!(explained["capacity"]["wrapper_leaf_runs"], 1);
    assert_eq!(explained["capacity"]["free_slots"], 1);
    assert_eq!(
        explained["capacity"]["leaf_occupancy_by_pipeline"]["task_claimed_local_pipeline"],
        1
    );

    // The last slot goes to an admission that has no run at all yet.
    let parent = jobs
        .insert_job_run("workspace_auto_pipeline", 1, Utc::now(), None, None)
        .expect("drain run");
    jobs.write_run_state(
        &parent.run_id,
        &orbit_types::workflow::PipelineState::new(
            parent.run_id.clone(),
            parent.job_id.clone(),
            json!({}),
        ),
    )
    .expect("drain state");
    jobs.allocate_pull_request(
        &orbit_store::contracts::PullDestination {
            owner_machine_id: "owner".into(),
            owner_workspace_id: runtime.workspace_id().expect("workspace"),
            selector: "owner/ws".into(),
            execution_machine_id: "owner".into(),
        },
        &orbit_store::contracts::AdmissionRequest {
            request_id: "pending".into(),
            caller_version: "1".into(),
            caller_schema: 1,
            caller_review_policy: "none".into(),
            run_context: orbit_store::contracts::AdmissionRunContext {
                run_id: parent.run_id,
                job_name: "workspace_auto_pipeline".into(),
                machine_name: None,
            },
            ship: orbit_store::contracts::AdmissionShipContract {
                mode: "local".into(),
                base_branch: "main".into(),
                landing_branch: "main".into(),
                review_policy: "none".into(),
                completion: "review".into(),
                authorization_reference: None,
            },
        },
        3,
    )
    .expect("allocate")
    .expect("the third slot was free");

    let saturated = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
    assert_eq!(saturated["active_leaf_runs"], 3);
    assert_eq!(saturated["free_slots"], 0);
    assert_eq!(
        saturated["leaf_occupancy_by_pipeline"]["task_claimed_local_pipeline"], 2,
        "the pending admission counts against its definition as well as the ceiling"
    );
    assert_eq!(saturated["loose_task_ids"], json!([]));
    assert_eq!(
        saturated["idle"], true,
        "a drain saturated by claimed work admits nothing this pass"
    );
    assert!(
        saturated["pending_backlog"].as_u64().expect("pending") > 0,
        "saturation is not an empty backlog"
    );
}

/// [ORB-12649] Shared occupancy counts non-wrapper leaf rows that the
/// classifier used to leave unreconciled. An orphaned `task_pr_pipeline` (or
/// claimed) worker would eat a slot forever; a drain iteration must regain it
/// without an external query or workspace reopen.
#[test]
fn an_orphaned_non_wrapper_leaf_does_not_consume_a_drain_slot() {
    for job_name in [
        "task_pr_pipeline",
        "task_claimed_pr_pipeline",
        "task_claimed_local_pipeline",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let healthy = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
        assert_eq!(healthy["free_slots"], 3, "{job_name}: healthy free_slots");
        assert_eq!(
            healthy["active_leaf_runs"], 0,
            "{job_name}: healthy occupancy"
        );

        let jobs = runtime.stores().jobs();
        let orphan = jobs
            .insert_job_run(job_name, 1, Utc::now(), None, None)
            .expect("insert orphaned leaf");
        jobs.mark_job_run_running(
            &orphan.run_id,
            Utc::now() - chrono::Duration::seconds(3),
            999_999,
        )
        .expect("mark orphaned leaf running with a dead pid");

        let occupancy_before = jobs
            .drain_leaf_occupancy()
            .expect("raw occupancy before classify");
        assert_eq!(
            occupancy_before.occupied, 1,
            "{job_name} must occupy a slot on the raw reading"
        );

        let recovered = classify_with(&runtime, json!({ "max_active_leaf_runs": 3 }));
        assert_eq!(
            recovered["free_slots"], healthy["free_slots"],
            "{job_name}: drain iteration must regain the orphaned slot"
        );
        assert_eq!(
            recovered["active_leaf_runs"], 0,
            "{job_name}: occupied after classify"
        );
        assert_eq!(
            recovered["leaf_occupancy_by_pipeline"]
                .get(job_name)
                .and_then(Value::as_u64)
                .unwrap_or(0),
            0,
            "{job_name} must not remain in the occupancy breakdown"
        );

        let stored = runtime
            .get_job_run_backend(&orphan.run_id)
            .expect("read orphaned run")
            .expect("orphaned run exists");
        assert_eq!(
            stored.state,
            JobRunState::Interrupted,
            "{job_name} must be finalized by the drain iteration"
        );
    }
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

/// An `epic`-tagged root goes through the same effective-crew rule as any other
/// leaf, so a restricted window withholds it exactly as it withholds a chore
/// [ORB-12491].
#[test]
fn crew_allowlist_withholds_an_excluded_tagged_root() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_config(Some(ALLOWLIST_CREW_CONFIG));
    let tagged_root = seed_list_backlog_task(
        &runtime,
        "Excluded large task",
        TaskStatus::Backlog,
        TaskPriority::Medium,
        TaskType::Feature,
        None,
        vec![],
    );
    runtime
        .update_task(
            &tagged_root.id,
            TaskUpdateParams {
                crew: Some(Some("fable".to_string())),
                tags: Some(vec!["epic".to_string()]),
                ..Default::default()
            },
        )
        .expect("tag root");

    assert_eq!(
        classify_with(&runtime, json!({ "allowed_crews": ["opus"] }))["loose_task_ids"],
        json!([])
    );
    assert_eq!(
        classify(&runtime)["loose_task_ids"],
        json!([tagged_root.id])
    );
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
fn a_stopped_drain_admits_nothing_and_leaves_live_children() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    seed_backlog_leaves(&runtime, 1);
    let tagged_root = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Tagged root".to_string(),
            description: "fixture".to_string(),
            acceptance_criteria: vec!["fixture".to_string()],
            plan: "fixture".to_string(),
            tags: vec!["epic".to_string()],
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Hard,
            ..Default::default()
        })
        .expect("seed tagged root");
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
    assert_eq!(output["has_leaves"], false);
    assert!(
        !output["loose_task_ids"]
            .as_array()
            .expect("admitted")
            .contains(&json!(tagged_root.id)),
        "a stopped drain must not start an admissible large task: {output}"
    );
    let child = runtime.show_job_run(&live).expect("show child");
    assert!(
        !child.state.is_terminal(),
        "stop must not cancel an already admitted child"
    );

    let readiness = readiness(&runtime, &[], None);
    assert_eq!(readiness["capacity"]["admissions_stopped"], true);
    assert_eq!(readiness["capacity"]["free_slots"], 0);
}

/// [ORB-12968] A drain wave taken while the host has a reboot scheduled admits
/// nothing and names the schedule; readiness gives every waiting task the same
/// reason. A live child is left running, and once the schedule clears the next
/// wave admits again on its own.
#[test]
fn a_scheduled_host_shutdown_holds_drain_waves_and_leaves_live_children() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "crates/leaf_0/src/lib.rs");
    let leaves = seed_backlog_leaves(&runtime, 1);
    let live = seed_live_leaf_run(&runtime, &["CARRIED"]);
    let held =
        runtime
            .clone()
            .with_host_signal_probe(std::sync::Arc::new(FixedHostSignals::scheduled(
                ScheduledShutdown {
                    mode: "reboot".to_string(),
                    scheduled_at: Utc
                        .with_ymd_and_hms(2026, 9, 25, 4, 0, 0)
                        .single()
                        .expect("time"),
                    source: "fixture".to_string(),
                },
            )));

    let output = classify_with(&held, json!({ "max_active_leaf_runs": 5 }));
    assert_eq!(output["free_slots"], 0, "{output}");
    assert_eq!(output["has_leaves"], false, "{output}");
    assert_eq!(output["host_shutdown"]["mode"], "reboot", "{output}");
    assert_eq!(
        output["host_shutdown"]["scheduled_at"], "2026-09-25T04:00:00Z",
        "{output}"
    );
    assert!(
        !runtime
            .show_job_run(&live)
            .expect("show child")
            .state
            .is_terminal(),
        "the hold must not cancel an already admitted child"
    );

    let explained = readiness(&held, &[], None);
    assert_eq!(explained["capacity"]["free_slots"], 0);
    assert_eq!(explained["capacity"]["host_shutdown"]["mode"], "reboot");
    let entry = readiness_task(&explained, &leaves[0]);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "host_shutdown_scheduled", "{entry}");

    let resumed = classify_with(&runtime, json!({ "max_active_leaf_runs": 5 }));
    assert!(resumed["host_shutdown"].is_null(), "{resumed}");
    assert_eq!(resumed["loose_task_ids"], json!([leaves[0]]), "{resumed}");
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

#[test]
fn readiness_separates_a_queued_drain_from_the_running_coordinator() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let running = seed_running_drain_input(
        &runtime,
        json!({ "max_active_leaf_runs": 3, "completion": "done" }),
    );
    let queued = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "max_active_leaf_runs": 5, "completion": "review" })),
            None,
        )
        .expect("queue drain");

    let output = readiness(&runtime, &[], None);
    assert_eq!(output["capacity"]["drain_run_id"], running);
    assert_eq!(output["capacity"]["max_active_leaf_runs"], 3);
    assert_eq!(output["capacity"]["limit_source"], "run_input");
    assert_eq!(
        output["capacity"]["queued_drains"],
        json!([{ "run_id": queued.run_id, "max_active_leaf_runs": 5, "completion": "review" }])
    );
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
    // [ORB-12717] assigns a crew at creation, so the dispatch-time pool route
    // this covers is only reachable by a record filed before that change.
    let unassigned = runtime
        .add_crew_less_task_for_tests(TaskAddParams {
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

/// [ORB-12604] A crew parked at weight 0 holds no ticket, so it cannot make a
/// task eligible for a drain restricted to it. [ORB-12717] The draw now runs
/// when the task is created, so the parked member never becomes the task's
/// crew and a drain restricted to it excludes the task as `crew_not_allowed`.
#[test]
fn a_pool_member_parked_at_weight_zero_is_not_a_permitted_crew() {
    let (_root, runtime, _repo) = runtime_with_workspace_config(Some(
        "[workflow]\ndefault_crew = \"opus\"\nmedium_complexity_crews = [\"grok:0\", \"terra:50\"]\n",
    ));
    let task = runtime
        .add_task(TaskAddParams {
            title: "Weighted pool task".into(),
            description: "Only the weighted member can be drawn".into(),
            plan: "Inspect classification".into(),
            complexity: orbit_types::task::TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("task");
    assert_eq!(
        task.crew.as_deref(),
        Some("terra"),
        "the parked member holds no ticket in the creation-time draw"
    );
    assert_eq!(
        classify_with(&runtime, json!({"allowed_crews": ["terra"]}))["loose_task_ids"],
        json!([task.id])
    );
    assert_eq!(
        classify_with(&runtime, json!({"allowed_crews": ["grok"]}))["loose_task_ids"],
        json!([])
    );
    let readiness = runtime
        .workspace_auto_readiness(std::slice::from_ref(&task.id), None, 10, &["grok".into()])
        .expect("readiness");
    let excluded = readiness_task(&readiness, &task.id);
    assert_eq!(excluded["reason"], "crew_not_allowed");
    assert_eq!(excluded["crew"], "terra");
}

#[test]
fn classifier_uses_captured_cli_pool_instead_of_current_configuration() {
    let (_root, runtime, _repo) =
        runtime_with_workspace_config(Some("[workflow]\nmedium_complexity_crews = [\"grok\"]\n"));
    let task = runtime
        .add_crew_less_task_for_tests(TaskAddParams {
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

#[test]
fn readiness_agrees_with_admission_on_the_no_diff_expected_complexity_exemption() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let exempt = seed_unassessed_task(&runtime, "Operational check", &["no-diff-expected"]);
    let implementation = seed_unassessed_task(&runtime, "Repair the finding", &["code-review"]);
    let blocked_dependency = seed_list_backlog_task(
        &runtime,
        "Unfinished dependency",
        TaskStatus::InProgress,
        TaskPriority::Medium,
        TaskType::Chore,
        None,
        vec![],
    );
    let dependent_exempt = runtime
        .add_task(TaskAddParams {
            title: "Operational check behind a dependency".to_string(),
            description: "Fixture task".to_string(),
            acceptance_criteria: vec!["Fixture outcome is observable.".to_string()],
            dependencies: vec![blocked_dependency.id.clone()],
            tags: vec!["no-diff-expected".to_string()],
            plan: "Fixture plan.".to_string(),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed dependent exempt task");

    let output = readiness(
        &runtime,
        &[
            exempt.id.clone(),
            implementation.id.clone(),
            dependent_exempt.id.clone(),
        ],
        None,
    );

    assert_eq!(readiness_task(&output, &exempt.id)["eligible"], json!(true));
    assert_eq!(readiness_task(&output, &exempt.id)["reason"], "ready");
    assert_eq!(
        readiness_task(&output, &implementation.id)["reason"],
        "task_pilot_preparation_required"
    );
    assert_eq!(
        readiness_task(&output, &implementation.id)["eligible"],
        json!(false)
    );
    // Other exclusions stay visible and enforced for an exempt task.
    assert_eq!(
        readiness_task(&output, &dependent_exempt.id)["reason"],
        "unmet_dependency"
    );
    assert_eq!(
        readiness_task(&output, &dependent_exempt.id)["eligible"],
        json!(false)
    );
    // Admission itself never rewrites the stored non-answer.
    assert_eq!(
        runtime
            .get_task(&exempt.id)
            .expect("exempt task")
            .complexity,
        Some(TaskComplexity::Unassessed)
    );
}

fn seed_unassessed_task(runtime: &OrbitRuntime, title: &str, tags: &[&str]) -> Task {
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
