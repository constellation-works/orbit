//! Backlog work that declares no footprint [ORB-15191].
//!
//! A backlog task with no `context_files` holds no file lock. Three such tasks
//! promoted into a live four-slot drain used to run concurrently and all edit
//! one stylesheet. Now a multi-slot drain or ship waits for the task pilot to
//! persist selectors, and when no pilot will, runs the task only alone. A
//! single-slot drain and an explicitly selected ship keep admitting it.

use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

use super::{
    Seed, admitted, as_operator, isolated, list_backlog_tasks, readiness_task, running_run,
    runtime, seed, write_files,
};

const MACHINE: &str = "fixture-machine";

/// A state-triggered task pilot this host owns, as `orbit routine` declares it.
fn install_pilot(repo: &std::path::Path) {
    let routines = repo.join(".orbit/routines");
    std::fs::create_dir_all(&routines).unwrap();
    let routine = json!({
        "schemaVersion": 1, "name": "fixture-pilot", "enabled": true,
        "target": "job:task_pilot_pipeline",
        "trigger": {"state": {
            "kind": "preparation_eligible", "owner_machine": MACHINE, "branch": "main",
            "debounce_minutes": 2, "max_wait_minutes": 10, "max_items": 50,
            "retries": 1, "deadline_minutes": 90,
        }},
    });
    std::fs::write(routines.join("fixture-pilot.yaml"), routine.to_string()).unwrap();
}

fn empty(runtime: &OrbitRuntime, title: &str) -> String {
    seed(
        runtime,
        Seed {
            title,
            context_files: Some(&[]),
            ..Seed::default()
        },
    )
    .id
}

fn classify_with(runtime: &OrbitRuntime, drain: &str, slots: u32) -> Value {
    runtime
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": slots}),
            ToolContext::default(),
        )
        .expect("classify")
}

fn deferred<'a>(wave: &'a Value, task: &str) -> &'a Value {
    wave["deferred_conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap_or_else(|| panic!("{task} is not deferred: {wave:#}"))
}

fn set_context(runtime: &OrbitRuntime, task: &str, selectors: &[&str]) {
    runtime
        .update_task_as_human(
            task,
            TaskUpdateParams {
                context_files: Some(selectors.iter().map(ToString::to_string).collect()),
                ..Default::default()
            },
            "fixture operator".into(),
        )
        .unwrap();
}

/// Append the receipt a task-pilot assessment leaves, as the pilot does.
fn record_pilot_assessment(runtime: &OrbitRuntime, task: &str) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&runtime.global_root()),
    )
    .unwrap();
    let at = chrono::Utc::now();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .history
    .update_task_history(
        task,
        orbit_store::contracts::TaskHistoryUpdateParams {
            actor: "task-pilot".into(),
            append_comments: vec![orbit_types::task::TaskComment {
                at,
                by: "task-pilot".into(),
                message: "operation_id=fixture-pilot\n{\"assessment\":{}}".into(),
            }],
            ..Default::default()
        },
    )
    .unwrap();
}

/// With a pilot that will prepare them, two empty-context backlog tasks wait
/// for it in a multi-slot drain, its readiness and a discovery ship, and run
/// under the ordinary lock rules once they carry selectors. A single-slot
/// drain and an explicit ship admit them as before. Once the pilot has
/// assessed a task and left it without selectors, it stops waiting and goes
/// only alone.
#[test]
fn a_multi_slot_drain_waits_for_the_pilot_to_prepare_empty_context_work() {
    if !isolated(
        "dispatch_admission::unknown_footprint::a_multi_slot_drain_waits_for_the_pilot_to_prepare_empty_context_work",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    let runtime = runtime.with_automation_machine_identity(Some(MACHINE.into()));
    install_pilot(&repo);
    let first = empty(&runtime, "restyle cards");
    let second = empty(&runtime, "restyle code");
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    let wave = classify_with(&runtime, &drain, 2);
    assert_eq!(wave["loose_task_ids"], json!([]), "{wave:#}");
    for task in [&first, &second] {
        let entry = deferred(&wave, task);
        assert_eq!(entry["reason"], "awaiting_footprint", "{entry}");
        assert!(entry["detail"].is_string(), "{entry}");
    }
    let shown = as_operator(&runtime, "orbit.workflow.run.show", json!({"id": drain}));
    let waiting = shown["drain_last_pass"]["deferred"].as_array().unwrap();
    assert_eq!(waiting.len(), 2, "{shown:#}");
    assert!(
        waiting
            .iter()
            .all(|entry| entry["reason"] == "awaiting_footprint"),
        "{shown:#}"
    );
    let readiness = runtime
        .workspace_auto_readiness(&[], Some(2), 50, &[])
        .unwrap();
    for task in [&first, &second] {
        let entry = readiness_task(&readiness, task);
        assert_eq!(entry["reason"], "awaiting_footprint", "{entry}");
        assert_ne!(entry["eligible"], true, "{entry}");
    }
    let discovery = list_backlog_tasks(&runtime, json!({}));
    assert_eq!(admitted(&discovery), Vec::<String>::new(), "{discovery:#}");
    for task in [&first, &second] {
        assert_eq!(
            super::excluded_entry(&discovery, task)["reason"],
            "awaiting_footprint",
            "{discovery:#}"
        );
    }

    // An explicitly selected ship and a one-slot drain keep today's rule.
    let explicit = list_backlog_tasks(&runtime, json!({"task_ids": [first, second]}));
    assert_eq!(
        admitted(&explicit),
        vec![first.clone(), second.clone()],
        "{explicit:#}"
    );
    let single = classify_with(&runtime, &drain, 1);
    assert_eq!(single["loose_task_ids"], json!([first]), "{single:#}");

    // Selectors persisted: ordinary lock rules, overlapping then disjoint.
    write_files(&repo, &["site/custom.css", "site/code.css"]);
    set_context(&runtime, &first, &["file:site/custom.css"]);
    set_context(&runtime, &second, &["file:site/custom.css"]);
    let overlapping = classify_with(&runtime, &drain, 2);
    assert_eq!(
        overlapping["loose_task_ids"],
        json!([first]),
        "{overlapping:#}"
    );
    let entry = deferred(&overlapping, &second);
    assert_eq!(entry["reason"], "conflict_deferred", "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([first]), "{entry}");
    set_context(&runtime, &second, &["file:site/code.css"]);
    let disjoint = classify_with(&runtime, &drain, 2);
    assert_eq!(
        disjoint["loose_task_ids"],
        json!([first, second]),
        "{disjoint:#}"
    );

    // Assessed and still empty: the pilot declined, so the task stops
    // waiting and goes alone, behind the work ranked ahead of it.
    let declined = empty(&runtime, "restyle footer");
    let waiting = classify_with(&runtime, &drain, 3);
    assert_eq!(
        deferred(&waiting, &declined)["reason"],
        "awaiting_footprint",
        "{waiting:#}"
    );
    record_pilot_assessment(&runtime, &declined);
    let assessed = classify_with(&runtime, &drain, 3);
    assert_eq!(
        assessed["loose_task_ids"],
        json!([first, second]),
        "{assessed:#}"
    );
    let entry = deferred(&assessed, &declined);
    assert_eq!(entry["reason"], "awaiting_exclusive_slot", "{entry}");
    assert_eq!(entry["conflicts"], json!([]), "{entry}");
}

/// With no pilot, empty-context work is a whole-tree footprint: it is admitted
/// only when nothing else editing is in flight or ahead of it in the wave,
/// blocks other editing work while it runs, and, when it must wait, reserves
/// the tree so lower-ranked editing work cannot keep it waiting. Work tagged
/// `no-diff-expected` neither waits nor is held.
#[test]
fn without_a_pilot_empty_context_work_is_admitted_only_alone() {
    if !isolated(
        "dispatch_admission::unknown_footprint::without_a_pilot_empty_context_work_is_admitted_only_alone",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    let first = empty(&runtime, "restyle cards");
    let second = empty(&runtime, "restyle code");
    let editing = seed(&runtime, Seed::default()).id;
    let no_diff = seed(
        &runtime,
        Seed {
            title: "side effects only",
            context_files: Some(&[]),
            tags: &["no-diff-expected"],
            ..Seed::default()
        },
    )
    .id;
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    // Nothing in flight: the first goes alone, holding the tree in its wave.
    let wave = classify_with(&runtime, &drain, 4);
    assert_eq!(wave["loose_task_ids"], json!([first, no_diff]), "{wave:#}");
    for task in [&second, &editing] {
        let entry = deferred(&wave, task);
        assert_eq!(entry["reason"], "conflict_deferred", "{entry}");
        assert_eq!(entry["blocking_task_ids"], json!([first]), "{entry}");
        assert_eq!(entry["conflicts"][0]["provenance"], "same_wave", "{entry}");
    }
    let readiness = runtime
        .workspace_auto_readiness(&[], Some(4), 50, &[])
        .unwrap();
    assert_eq!(readiness_task(&readiness, &first)["reason"], "ready");
    let entry = readiness_task(&readiness, &second);
    assert_eq!(entry["reason"], "conflict_deferred", "{entry}");
    assert_eq!(entry["blocking_task_ids"], json!([first]), "{entry}");

    // While it runs it holds the tree.
    running_run(&runtime, "task_auto_pipeline", json!({"task_ids": [first]}));
    let held = classify_with(&runtime, &drain, 4);
    assert_eq!(held["loose_task_ids"], json!([no_diff]), "{held:#}");
    for task in [&second, &editing] {
        let entry = deferred(&held, task);
        assert_eq!(entry["blocking_task_ids"], json!([first]), "{entry}");
        assert_eq!(
            entry["conflicts"][0]["provenance"], "unknown_footprint",
            "{entry}"
        );
    }

    // Once the running task declares a footprint, a leaf is still in
    // flight: the next empty-context task waits for an exclusive slot and
    // reserves the tree, so the editing task ranked behind it waits too.
    write_files(&repo, &["src/lib.rs"]);
    set_context(&runtime, &first, &["dir:src"]);
    let reserved = classify_with(&runtime, &drain, 4);
    assert_eq!(reserved["loose_task_ids"], json!([no_diff]), "{reserved:#}");
    let entry = deferred(&reserved, &second);
    assert_eq!(entry["reason"], "awaiting_exclusive_slot", "{entry}");
    assert!(entry["detail"].is_string(), "{entry}");
    let entry = deferred(&reserved, &editing);
    assert_eq!(entry["blocking_task_ids"], json!([second]), "{entry}");
    assert_eq!(
        entry["conflicts"][0]["provenance"], "exclusive_reservation",
        "{entry}"
    );
    let readiness = runtime
        .workspace_auto_readiness(&[], Some(4), 50, &[])
        .unwrap();
    let entry = readiness_task(&readiness, &second);
    assert_eq!(entry["reason"], "awaiting_exclusive_slot", "{entry}");
    let discovery = list_backlog_tasks(&runtime, json!({}));
    assert!(!admitted(&discovery).contains(&second), "{discovery:#}");
    assert_eq!(
        super::excluded_entry(&discovery, &second)["reason"],
        "awaiting_exclusive_slot",
        "{discovery:#}"
    );
}
