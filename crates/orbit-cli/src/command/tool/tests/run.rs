use std::sync::OnceLock;

use serde_json::json;

use super::super::run::{
    LOCAL_MACHINE_ID_FALLBACK, ToolRunArgs, local_machine_identity, local_tool_session_context,
    shape_tool_output,
};

#[test]
fn id_resolved_task_id_is_read_from_id_resolved_tool_input() {
    let show = ToolRunArgs {
        name: "orbit.task.show".to_string(),
        input: Some(r#"{"id":" ORB-10961 ","model":"codex"}"#.to_string()),
        input_file: None,
        agent: None,
        model: None,
        dry_run: false,
        fields: Vec::new(),
        full: false,
        pretty: false,
        parsed_input: OnceLock::new(),
    };
    assert_eq!(show.id_resolved_task_id().as_deref(), Some("ORB-10961"));

    let artifact_get = ToolRunArgs {
        name: "orbit.task.artifact.get".to_string(),
        input: Some(r#"{"id":" ORB-12263 ","path":"qa/note.md"}"#.to_string()),
        input_file: None,
        agent: None,
        model: None,
        dry_run: false,
        fields: Vec::new(),
        full: false,
        pretty: false,
        parsed_input: OnceLock::new(),
    };
    assert_eq!(
        artifact_get.id_resolved_task_id().as_deref(),
        Some("ORB-12263")
    );

    let list = ToolRunArgs {
        name: "orbit.task.list".to_string(),
        input: Some(r#"{"id":"ORB-10961"}"#.to_string()),
        ..show
    };
    assert_eq!(list.id_resolved_task_id(), None);
}

#[test]
fn list_output_uses_minimal_task_projection() {
    let shaped = shape_tool_output(
        "orbit.task.list",
        json!({
            "tasks": [{
                "id": "T20260422-0001",
                "title": "Backlog task",
                "status": "backlog",
                "priority": "medium",
                "type": "feature",
                "dependencies": [],
                "resolved_dependencies": [],
                "implemented_by": null,
                "created_at": "2026-04-22T00:00:00Z",
                "updated_at": "2026-04-22T00:00:00Z",
                "description": "should be filtered out"
            }],
            "total": 1,
            "truncated": false
        }),
        false,
        &[],
    );

    assert_eq!(
        shaped,
        json!({
            "tasks": [{
                "id": "T20260422-0001",
                "title": "Backlog task",
                "status": "backlog",
                "priority": "medium",
                "type": "feature",
                "dependencies": [],
                "resolved_dependencies": [],
                "implemented_by": null,
                "created_at": "2026-04-22T00:00:00Z",
                "updated_at": "2026-04-22T00:00:00Z"
            }],
            "total": 1,
            "truncated": false
        })
    );
}

#[test]
fn list_output_projects_explicit_fields_inside_envelope() {
    let shaped = shape_tool_output(
        "orbit.task.list",
        json!({
            "tasks": [{
                "id": "T20260422-0001",
                "title": "Backlog task",
                "status": "backlog",
                "description": "should be filtered out"
            }],
            "total": 1,
            "truncated": false
        }),
        false,
        &["id".to_string(), "title".to_string()],
    );

    assert_eq!(
        shaped,
        json!({
            "tasks": [{
                "id": "T20260422-0001",
                "title": "Backlog task"
            }],
            "total": 1,
            "truncated": false
        })
    );
}

#[test]
fn add_update_approve_start_preserve_full_task_by_default() {
    let output = json!({
        "id": "T20260422-0001",
        "title": "Full record",
        "status": "backlog",
        "priority": "medium",
        "type": "feature",
        "complexity": "low",
        "tags": ["qa"],
        "context_files": ["file:src/lib.rs"],
        "created_by": "codex",
        "crew": "grok",
        "orchestrator": null,
        "plan": "keep the full envelope",
        "comments": [],
        "history": [],
        "redactions": [],
        "redactions_applied": false,
        "created_at": "2026-04-22T00:00:00Z",
        "updated_at": "2026-04-22T00:00:00Z"
    });

    for tool in [
        "orbit.task.add",
        "orbit.task.update",
        "orbit.task.approve",
        "orbit.task.start",
    ] {
        assert_eq!(
            shape_tool_output(tool, output.clone(), false, &[]),
            output,
            "{tool} must default to the full task record"
        );
    }
}

#[test]
fn add_output_still_projects_explicit_fields() {
    let shaped = shape_tool_output(
        "orbit.task.add",
        json!({
            "id": "T20260422-0001",
            "title": "Full record",
            "status": "proposed",
            "complexity": "low"
        }),
        false,
        &["id".to_string(), "title".to_string()],
    );
    assert_eq!(
        shaped,
        json!({
            "id": "T20260422-0001",
            "title": "Full record"
        })
    );
}

#[test]
fn show_output_preserves_task_details_by_default() {
    let output = json!({
        "id": "T20260422-0001",
        "title": "Task details",
        "status": "backlog",
        "priority": "medium",
        "type": "feature",
        "dependencies": [],
        "resolved_dependencies": [],
        "implemented_by": null,
        "created_at": "2026-04-22T00:00:00Z",
        "updated_at": "2026-04-22T00:00:00Z",
        "description": "details must remain available",
        "acceptance_criteria": ["inspect the complete task"]
    });

    assert_eq!(
        shape_tool_output("orbit.task.show", output.clone(), false, &[],),
        output
    );
}

#[test]
fn local_invocation_context_has_trace_and_explicit_identity_fallback() {
    let runtime = orbit_core::OrbitRuntime::in_memory().expect("in-memory runtime");

    let context = local_tool_session_context(&runtime, None).expect("local invocation context");

    assert!(
        context
            .trace_id
            .as_deref()
            .is_some_and(|trace| trace.starts_with("trace-"))
    );
    assert_eq!(
        context.caller_machine_id.as_deref(),
        Some(LOCAL_MACHINE_ID_FALLBACK)
    );
    assert_eq!(
        context.process_machine_id.as_deref(),
        Some(LOCAL_MACHINE_ID_FALLBACK)
    );
    assert_eq!(context.caller_ip, None);
    assert!(context.effective_capabilities.is_empty());
}

#[test]
fn parsed_input_reads_an_input_file_once() {
    let file = tempfile::NamedTempFile::new().expect("input file");
    std::fs::write(file.path(), r#"{"id":"first"}"#).expect("write initial input");
    let show = ToolRunArgs {
        name: "orbit.task.show".to_string(),
        input: None,
        input_file: Some(file.path().to_string_lossy().into_owned()),
        agent: None,
        model: None,
        dry_run: false,
        fields: Vec::new(),
        full: false,
        pretty: false,
        parsed_input: OnceLock::new(),
    };

    assert_eq!(
        show.parsed_input().expect("first parse"),
        json!({"id":"first"})
    );
    std::fs::write(file.path(), r#"{"id":"second"}"#).expect("write replacement input");
    assert_eq!(
        show.parsed_input().expect("cached parse"),
        json!({"id":"first"})
    );
}

#[test]
fn local_machine_identity_prefers_persisted_host_identity() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_cli\"\nhost_id = \"cli-host\"\ntask_prefix = \"CLI\"\n",
    )
    .expect("write host identity");

    let identity = local_machine_identity(root.path()).expect("load local machine identity");

    assert_eq!(
        identity,
        ("hm_cli".to_string(), Some("cli-host".to_string()))
    );
}
