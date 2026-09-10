//! Folding a pre-grouping backlog of per-alert tasks into remediation groups.
//!
//! The source tasks here are minted by the sweep itself, one alert at a time,
//! which is exactly the shape a backlog swept before grouping still holds.

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{TaskPriority, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::dependabot_alert_tasks::{code_alert, expanded_snapshot, file};

const RECONCILE: &str = "crates/orbit-core/src/runtime/reconcile.rs";
const SALT: &str = "The hard-coded value is used as a salt for this hash.";

fn hash_alert(number: u64, line: u64) -> Value {
    let mut alert = code_alert(number, "high");
    alert["rule_id"] = json!("rust/hard-coded-cryptographic-value");
    alert["rule_name"] = json!("Hard-coded cryptographic value");
    alert["message"] = json!(SALT);
    alert["path"] = json!(RECONCILE);
    alert["start_line"] = json!(line);
    alert["end_line"] = json!(line);
    alert
}

/// Sweep one alert at a time so each lands as its own per-alert task, the way
/// the sweep filed them before it grouped by shared cause.
fn seed_per_alert_tasks(runtime: &OrbitRuntime, alerts: &[Value]) -> Vec<String> {
    alerts
        .iter()
        .map(|alert| {
            let output = file(
                runtime,
                expanded_snapshot(Vec::new(), vec![alert.clone()], Vec::new()),
                json!({}),
            );
            assert_eq!(output["filed_count"], json!(1));
            output["filed"][0]["task_id"]
                .as_str()
                .expect("task id")
                .to_string()
        })
        .collect()
}

fn consolidate(runtime: &OrbitRuntime, input: Value) -> Value {
    runtime
        .run_deterministic(
            "consolidate_code_scanning_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("consolidate code scanning tasks")
}

fn seeded_reconcile_backlog() -> (tempfile::TempDir, OrbitRuntime, Vec<String>) {
    let (root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);
    let sources = seed_per_alert_tasks(
        &runtime,
        &[
            hash_alert(101, 118),
            hash_alert(102, 246),
            hash_alert(103, 426),
        ],
    );
    (root, runtime, sources)
}

#[test]
fn a_dry_run_names_the_exact_source_tasks_and_writes_nothing() {
    let (_root, runtime, sources) = seeded_reconcile_backlog();
    let before = runtime.list_tasks().expect("tasks").len();

    let preview = consolidate(&runtime, json!({}));

    assert_eq!(preview["outcome"], "dry_run");
    assert_eq!(preview["group_count"], json!(1));
    assert_eq!(preview["scanned_source_tasks"], json!(3));
    assert_eq!(preview["groups"][0]["source_task_ids"], json!(sources));
    assert_eq!(
        preview["groups"][0]["alert_numbers"],
        json!([101, 102, 103])
    );
    assert_eq!(preview["groups"][0]["paths"], json!([RECONCILE]));
    assert_eq!(
        preview["groups"][0]["rule_id"],
        "rust/hard-coded-cryptographic-value"
    );
    assert_eq!(preview["groups"][0]["replacement_priority"], "high");
    assert!(
        preview["groups"][0]["replacement_title"]
            .as_str()
            .expect("replacement title")
            .contains("3 locations")
    );

    assert_eq!(runtime.list_tasks().expect("tasks").len(), before);
    for source in &sources {
        assert_eq!(
            runtime.get_task(source).expect("source").status,
            TaskStatus::Backlog
        );
    }
}

#[test]
fn apply_replaces_the_sources_and_carries_their_traceability_and_dependencies() {
    let (_root, runtime, sources) = seeded_reconcile_backlog();
    let blocker = runtime
        .add_task(TaskAddParams {
            title: "Land the shared salt helper".to_string(),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed blocker")
        .id;
    runtime
        .update_task(
            &sources[1],
            TaskUpdateParams {
                dependencies: Some(vec![blocker.clone()]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("record a dependency on a source task");

    let applied = consolidate(&runtime, json!({"apply": true}));

    assert_eq!(applied["outcome"], "applied");
    assert_eq!(applied["groups"][0]["applied"], json!(true));
    assert_eq!(applied["groups"][0]["sources_not_retired"], json!([]));
    assert_eq!(
        applied["groups"][0]["carried_dependencies"],
        json!([blocker])
    );

    let replacement_id = applied["groups"][0]["replacement_task_id"]
        .as_str()
        .expect("replacement task id");
    let replacement = runtime.get_task(replacement_id).expect("replacement");
    assert_eq!(replacement.status, TaskStatus::Backlog);
    assert_eq!(replacement.priority, TaskPriority::High);
    assert_eq!(replacement.dependencies(), vec![blocker]);
    assert!(replacement.context_files.is_empty());
    for number in ["#101", "#102", "#103"] {
        assert!(
            replacement.description.contains(number),
            "alert {number} lost from the ledger"
        );
    }
    assert_eq!(
        replacement
            .tags
            .iter()
            .filter(|tag| tag.starts_with("code-scanning:"))
            .count(),
        3
    );
    assert!(
        replacement
            .relations
            .iter()
            .filter(|relation| relation.relation_type
                == orbit_types::task::TaskRelationType::Supersedes)
            .map(|relation| relation.target.clone())
            .eq(sources.iter().cloned()),
        "the replacement must supersede every source: {:?}",
        replacement.relations
    );

    for source in &sources {
        let retired = runtime.get_task(source).expect("source");
        assert_eq!(retired.status, TaskStatus::Rejected);
        let comments = runtime.get_task_comments(source).expect("comments");
        assert!(
            comments
                .iter()
                .any(|comment| comment.message.contains("covering task")
                    && comment.message.contains(replacement_id)),
            "a retired source must name its covering task: {comments:?}"
        );
    }
}

#[test]
fn consolidation_preserves_an_existing_prepared_selector() {
    let (_root, runtime, sources) = seeded_reconcile_backlog();
    runtime
        .update_task(
            &sources[0],
            TaskUpdateParams {
                context_files: Some(vec![format!("file:{RECONCILE}")]),
                ..TaskUpdateParams::default()
            },
        )
        .expect("persist a prepared source selector");

    let applied = consolidate(&runtime, json!({"apply": true}));
    let replacement_id = applied["groups"][0]["replacement_task_id"]
        .as_str()
        .expect("replacement id");
    let replacement = runtime.get_task(replacement_id).expect("replacement");

    assert_eq!(replacement.context_files, vec![format!("file:{RECONCILE}")]);
}

#[test]
fn applying_twice_changes_nothing_and_the_next_sweep_files_nothing() {
    let (_root, runtime, _sources) = seeded_reconcile_backlog();
    let first = consolidate(&runtime, json!({"apply": true}));
    assert_eq!(first["outcome"], "applied");
    let after_first = runtime.list_tasks().expect("tasks").len();

    let second = consolidate(&runtime, json!({"apply": true}));
    assert_eq!(second["outcome"], "nothing_to_consolidate");
    assert_eq!(second["groups"], json!([]));
    assert_eq!(runtime.list_tasks().expect("tasks").len(), after_first);
    assert!(
        second["skipped"]
            .as_array()
            .expect("skipped")
            .iter()
            .any(|entry| entry["reason"] == "already_grouped")
    );

    let resweep = file(
        &runtime,
        expanded_snapshot(
            Vec::new(),
            vec![
                hash_alert(101, 118),
                hash_alert(102, 246),
                hash_alert(103, 426),
            ],
            Vec::new(),
        ),
        json!({}),
    );
    assert_eq!(resweep["filed_count"], json!(0));
    assert_eq!(
        resweep["skipped_existing"]
            .as_array()
            .expect("skipped")
            .len(),
        3
    );
}

#[test]
fn started_deferred_and_unreadable_records_are_reported_and_left_alone() {
    let (_root, runtime, sources) = seeded_reconcile_backlog();
    runtime
        .update_task(
            &sources[0],
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..TaskUpdateParams::default()
            },
        )
        .expect("start a source task");
    let running = runtime.get_task(&sources[0]).expect("running source");

    let deferred = seed_per_alert_tasks(&runtime, &[hash_alert(104, 501)]).remove(0);
    runtime
        .update_task(
            &deferred,
            TaskUpdateParams {
                status: Some(TaskStatus::Someday),
                ..TaskUpdateParams::default()
            },
        )
        .expect("defer a source task");

    let unreadable = runtime
        .add_task(TaskAddParams {
            title: "[code-scanning-sweep] Hand-edited record".to_string(),
            description: "Somebody rewrote this record and dropped the evidence block.".to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:0123456789abcdef".to_string(),
            ],
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed an unreadable record")
        .id;

    let applied = consolidate(&runtime, json!({"apply": true}));

    let reasons = applied["skipped"]
        .as_array()
        .expect("skipped")
        .iter()
        .map(|entry| {
            (
                entry["task_id"].as_str().unwrap_or_default().to_string(),
                entry["reason"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert!(reasons.contains(&(sources[0].clone(), "active_work".to_string())));
    assert!(reasons.contains(&(deferred.clone(), "deferred".to_string())));
    assert!(reasons.contains(&(unreadable.clone(), "unparsable_evidence".to_string())));

    // Only the two untouched backlog records were folded.
    assert_eq!(
        applied["groups"][0]["source_task_ids"],
        json!([sources[1], sources[2]])
    );
    assert_eq!(applied["groups"][0]["alert_numbers"], json!([102, 103]));

    let after = runtime.get_task(&sources[0]).expect("running source");
    assert_eq!(after.status, TaskStatus::InProgress);
    assert_eq!(after.description, running.description);
    assert_eq!(after.tags, running.tags);
    assert_eq!(
        runtime.get_task(&deferred).expect("deferred").status,
        TaskStatus::Someday
    );
    assert_eq!(
        runtime.get_task(&unreadable).expect("unreadable").status,
        TaskStatus::Backlog
    );
}

#[test]
fn a_lone_task_for_its_cause_is_reported_unchanged() {
    let (_root, runtime, repo) = runtime_with_workspace_layout();
    write_workspace_file(&repo, RECONCILE);
    let mut other = hash_alert(200, 12);
    other["message"] = json!("The hard-coded value is used as a key for this cipher.");
    let sources = seed_per_alert_tasks(&runtime, &[hash_alert(101, 118), other]);

    let preview = consolidate(&runtime, json!({}));

    assert_eq!(preview["outcome"], "nothing_to_consolidate");
    let unchanged = preview["unchanged_single_source"]
        .as_array()
        .expect("unchanged")
        .iter()
        .map(|entry| entry["task_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(unchanged, sources);
}
