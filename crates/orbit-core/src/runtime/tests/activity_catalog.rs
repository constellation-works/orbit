//! Sibling tests for `activity_catalog.rs`: the activity catalog a runtime
//! resolves `target: activity:<name>` against.

use std::path::Path;

use super::runtime::test_runtime;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;

fn write_activity(path: &Path, name: &str, description: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: deterministic
  description: {description}
  action: test_action
  config: {{}}
"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

fn write_agent_loop_activity(path: &Path, name: &str, tools: &[&str]) {
    let tools_yaml = tools
        .iter()
        .map(|tool| format!("    - {tool}\n"))
        .collect::<String>();
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: agent_loop
  description: Test agent loop.
  instruction: Test.
  tools:
{tools_yaml}"#
    );
    std::fs::create_dir_all(path.parent().expect("activity path has parent"))
        .expect("create activity dir");
    std::fs::write(path, yaml).expect("write activity yaml");
}

#[test]
fn global_default_activity_wins_over_workspace_shadow_in_execution_catalog() {
    let (_root, runtime, global_root, workspace_root) = test_runtime();
    write_activity(
        &global_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "global description",
    );
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");
    let activity = catalog.get("pr_open").expect("pr_open activity");
    assert_eq!(activity.description, "global description");
}

#[test]
fn workspace_default_activity_cannot_claim_missing_global_default_name() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_activity(
        &workspace_root.join("resources/activities/pr_open.yaml"),
        "pr_open",
        "workspace description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(
        catalog.get("pr_open").is_none(),
        "workspace assets must never claim shipped default activity names"
    );
}

#[test]
fn activity_catalog_still_skips_retired_assets() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    std::fs::create_dir_all(&activities_dir).expect("create activities dir");
    std::fs::write(
        activities_dir.join("retired.yaml"),
        "schemaVersion: 1\nkind: Activity\nmetadata:\n  name: retired\nspec: {}\n",
    )
    .expect("write retired activity");
    write_activity(
        &activities_dir.join("current.yaml"),
        "current",
        "current description",
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("retired").is_none());
    assert!(catalog.get("current").is_some());
}

#[test]
fn duplicate_activities_within_one_catalog_directory_remain_invalid() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    let activities_dir = workspace_root.join("resources/activities");
    write_activity(
        &activities_dir.join("first.yaml"),
        "duplicate_activity",
        "first description",
    );
    write_activity(
        &activities_dir.join("nested/second.yaml"),
        "duplicate_activity",
        "second description",
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("duplicate activity name should fail");
    assert!(err.to_string().contains("duplicate activity name"), "{err}");
}

#[test]
fn activity_catalog_accepts_registered_task_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/task_tools.yaml"),
        "task_tools",
        &["orbit.task.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("task_tools").is_some());
}

#[test]
fn activity_catalog_rejects_unknown_concrete_tool() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/unknown_tool.yaml"),
        "unknown_tool",
        &["orbit.task.nope"],
    );

    let err = runtime
        .v2_activity_catalog()
        .expect_err("unknown concrete tool should fail");
    let message = err.to_string();

    assert!(message.contains("unknown_tool"), "{message}");
    assert!(message.contains("orbit.task.nope"), "{message}");
    assert!(message.contains("unknown tool name"), "{message}");
}

#[test]
fn activity_catalog_accepts_intentionally_empty_audit_wildcard() {
    let (_root, runtime, _global_root, workspace_root) = test_runtime();
    write_agent_loop_activity(
        &workspace_root.join("resources/activities/audit_tools.yaml"),
        "audit_tools",
        &["orbit.audit.*"],
    );

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert!(catalog.get("audit_tools").is_some());
}

#[test]
fn default_activity_catalog_allowlists_resolve_registered_tools() {
    let (_root, runtime, global_root, _workspace_root) = test_runtime();
    let activities_dir = global_root.join("resources/activities");
    for (name, yaml) in DEFAULT_ACTIVITY_FILES {
        let path = activities_dir.join(format!("{name}.yaml"));
        std::fs::create_dir_all(path.parent().expect("activity path has parent"))
            .expect("create activity dir");
        std::fs::write(path, yaml).expect("write activity yaml");
    }

    let catalog = runtime.v2_activity_catalog().expect("activity catalog");

    assert_eq!(catalog.len(), DEFAULT_ACTIVITY_FILES.len());
}
