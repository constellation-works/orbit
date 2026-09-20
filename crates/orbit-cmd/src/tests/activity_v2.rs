use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_core::application::SYSTEM_AUDIT_IDENTITY;
use orbit_store::contracts::V2AuditEventFilter;
use serde_json::json;
use tempfile::tempdir;

use super::super::activity_v2::ActivityV2Commands;

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn write_activity(path: &Path, name: &str) {
    let yaml = format!(
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: {name}
spec:
  type: deterministic
  description: Test deterministic sleep.
  action: sleep
  config: {{}}
"#
    );
    std::fs::write(path, yaml).expect("write activity yaml");
}

fn write_agent_loop_activity(path: &Path, name: &str, tool: &str) {
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
    - {tool}
"#
    );
    std::fs::write(path, yaml).expect("write activity yaml");
}

#[test]
fn direct_activity_run_uses_system_audit_identity() {
    let (_root, runtime, repo_root) = test_runtime();
    let yaml_path = repo_root.join("qa_activity_sleep.yaml");
    write_activity(&yaml_path, "qa_activity_sleep");

    let result = runtime
        .run_activity_v2_from_yaml(&yaml_path, json!({ "seconds": 0 }))
        .expect("direct activity run succeeds");

    let rows = runtime
        .list_v2_audit_events(V2AuditEventFilter {
            run_id: Some(result.run_id.clone()),
            ..Default::default()
        })
        .expect("list v2 audit events");
    let run_started = rows
        .iter()
        .find(|row| row.event_type == "run.started")
        .expect("run.started audit row");
    let first_event: serde_json::Value =
        serde_json::from_str(&run_started.payload_json).expect("parse run.started");
    assert_eq!(
        first_event
            .get("agent_identity")
            .and_then(serde_json::Value::as_str),
        Some(SYSTEM_AUDIT_IDENTITY)
    );
    assert_eq!(run_started.agent_identity, SYSTEM_AUDIT_IDENTITY);
}

#[test]
fn direct_activity_run_rejects_unknown_tool_before_dispatch() {
    let (_root, runtime, repo_root) = test_runtime();
    let yaml_path = repo_root.join("unknown_tool_activity.yaml");
    write_agent_loop_activity(&yaml_path, "unknown_tool_activity", "orbit.task.nope");

    let err = runtime
        .run_activity_v2_from_yaml(&yaml_path, json!({}))
        .expect_err("unknown tool should fail before dispatch");
    let message = err.to_string();

    assert!(message.contains("unknown_tool_activity"), "{message}");
    assert!(message.contains("orbit.task.nope"), "{message}");
    assert!(message.contains("unknown tool name"), "{message}");
}
