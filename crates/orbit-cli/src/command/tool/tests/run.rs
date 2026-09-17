use std::path::Path;
use std::sync::OnceLock;

use clap::Parser;
use serde_json::json;

use super::super::run::{
    LOCAL_MACHINE_ID_FALLBACK, ToolRunArgs, local_machine_identity, local_tool_session_context,
    missing_write_sidecar_message, request_write_sidecars_from_cli_fields, shape_tool_output,
};
use crate::command::{Cli, CommandOutput, Execute};

const UPDATE_HELP_GOLDENS_ENV: &str = "ORBIT_UPDATE_HELP_GOLDENS";

fn tool_run_args(name: &str, input: &str, fields: Vec<String>) -> ToolRunArgs {
    ToolRunArgs {
        name: name.to_string(),
        input: Some(input.to_string()),
        input_file: None,
        agent: None,
        model: Some("codex".to_string()),
        dry_run: false,
        fields,
        full: false,
        pretty: false,
        parsed_input: OnceLock::new(),
    }
}

fn payload_doc(output: crate::command::CommandOutput) -> serde_json::Value {
    let CommandOutput::Payload(payload) = output else {
        panic!("expected a document payload, got {output:?}");
    };
    payload.into_view().0
}

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
        shaped.expect("shape list output"),
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
        shaped.expect("shape list fields"),
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
fn add_and_update_are_not_cli_compacted_by_default() {
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

    for tool in ["orbit.task.add", "orbit.task.update"] {
        assert_eq!(
            shape_tool_output(tool, output.clone(), false, &[]).expect("shape write output"),
            output,
            "{tool} must not apply the CLI compact projection by default; \
             the tool omits comments/history unless projected"
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
        shaped.expect("shape add fields"),
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
        shape_tool_output("orbit.task.show", output.clone(), false, &[],)
            .expect("shape show output"),
        output
    );
}

#[test]
fn write_cli_fields_request_comments_and_history_from_the_tool() {
    let mut input = json!({"id": "T20260422-0001", "status": "review", "model": "codex"});
    request_write_sidecars_from_cli_fields(
        "orbit.task.update",
        &mut input,
        &["comments".to_string(), "history".to_string()],
    )
    .expect("merge sidecar fields into write input");
    assert_eq!(
        input,
        json!({
            "id": "T20260422-0001",
            "status": "review",
            "model": "codex",
            "fields": ["comments", "history"]
        })
    );
}

#[test]
fn write_cli_fields_union_sidecars_with_existing_tool_projection() {
    let mut input = json!({"id": "T20260422-0001", "field": "id"});
    request_write_sidecars_from_cli_fields(
        "orbit.task.update",
        &mut input,
        &["comments".to_string()],
    )
    .expect("union sidecar with existing field alias");
    assert_eq!(input["fields"], json!(["id", "comments"]));
    assert!(input.get("field").is_none());
}

#[test]
fn write_cli_fields_do_not_rewrite_input_without_sidecars() {
    let mut input = json!({"id": "T20260422-0001", "status": "review"});
    request_write_sidecars_from_cli_fields("orbit.task.update", &mut input, &["id".to_string()])
        .expect("id-only filter stays client-side");
    assert_eq!(input, json!({"id": "T20260422-0001", "status": "review"}));
}

#[test]
fn write_output_errors_when_cli_fields_name_an_omitted_sidecar() {
    let error = shape_tool_output(
        "orbit.task.update",
        json!({"id": "T20260422-0001", "status": "review"}),
        false,
        &["comments".to_string()],
    )
    .expect_err("missing comments must not collapse to {{}}");
    let message = error.to_string();
    assert!(
        message.contains(&missing_write_sidecar_message("comments")),
        "{message}"
    );
    assert!(message.contains(r#"--input '{"fields":["comments"]}'"#));
    assert!(message.contains(r#""field":"comments""#));
}

#[test]
fn write_single_field_sidecar_projection_is_rewrapped_for_cli_fields() {
    let shaped = shape_tool_output(
        "orbit.task.update",
        json!([{"message": "sidecar check"}]),
        false,
        &["comments".to_string()],
    )
    .expect("re-wrap unwrapped comments array");
    assert_eq!(shaped, json!({"comments": [{"message": "sidecar check"}]}));
}

#[test]
fn task_write_through_tool_run_returns_comments_and_history_when_projected() {
    let runtime = orbit_core::OrbitRuntime::in_memory().expect("in-memory runtime");

    let added = tool_run_args(
        "orbit.task.add",
        r#"{"title":"Write sidecar projection","description":"cover --fields comments","complexity":"low","model":"codex"}"#,
        vec![
            "id".to_string(),
            "comments".to_string(),
            "history".to_string(),
        ],
    )
    .execute(&runtime)
    .expect("orbit tool run orbit.task.add --fields id,comments,history");
    let added = payload_doc(added);
    assert!(
        added
            .get("comments")
            .and_then(|value| value.as_array())
            .is_some(),
        "add --fields comments must return comments, not {{}}: {added}"
    );
    assert!(
        added
            .get("history")
            .and_then(|value| value.as_array())
            .is_some(),
        "add --fields history must return history, not {{}}: {added}"
    );
    assert_eq!(
        added.as_object().map(|object| object.keys().count()),
        Some(3),
        "CLI --fields must still filter to the requested keys: {added}"
    );

    let task_id = added
        .get("id")
        .and_then(|value| value.as_str())
        .expect("added id")
        .to_string();
    let update_input = format!(r#"{{"id":"{task_id}","comment":"sidecar check","model":"codex"}}"#);
    let updated = tool_run_args(
        "orbit.task.update",
        &update_input,
        vec!["comments".to_string()],
    )
    .execute(&runtime)
    .expect("orbit tool run orbit.task.update --fields comments");
    let updated = payload_doc(updated);
    let comments = updated["comments"]
        .as_array()
        .unwrap_or_else(|| panic!("update --fields comments must return comments: {updated}"));
    assert!(
        comments
            .iter()
            .any(|comment| comment["message"] == "sidecar check"),
        "update must return the written comment: {updated}"
    );
}

#[test]
fn tool_run_help_matches_the_shipped_surface() {
    let actual = match Cli::try_parse_from(["orbit", "tool", "run", "--help"]) {
        Ok(_) => panic!("--help exits before parsing"),
        Err(error) => error.to_string(),
    };
    if std::env::var(UPDATE_HELP_GOLDENS_ENV).as_deref() == Ok("1") {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/command/tool/tests/run_help.txt");
        std::fs::write(&path, &actual)
            .unwrap_or_else(|err| panic!("write help golden {}: {err}", path.display()));
        return;
    }
    assert_eq!(
        actual,
        include_str!("run_help.txt"),
        "`orbit tool run --help` drifted from run_help.txt. If the new help is intentional, \
         regenerate with `{UPDATE_HELP_GOLDENS_ENV}=1 cargo test -p orbit-cli --bin orbit \
         tool_run_help_matches_the_shipped_surface` or `make goldens UPDATE=1`, then review the diff."
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

#[test]
fn infer_tool_name_preserves_dotted_names_without_known_script_extension() {
    use super::super::manifest::infer_tool_name;
    use std::path::Path;

    assert_eq!(infer_tool_name(Path::new("qa.echo")), "qa.echo");
    assert_eq!(
        infer_tool_name(Path::new("my.nested.tool.name")),
        "my.nested.tool.name"
    );
    assert_eq!(infer_tool_name(Path::new("/path/to/qa.echo")), "qa.echo");
}

#[test]
fn infer_tool_name_strips_known_script_extensions() {
    use super::super::manifest::infer_tool_name;
    use std::path::Path;

    assert_eq!(infer_tool_name(Path::new("qa.echo.py")), "qa.echo");
    assert_eq!(infer_tool_name(Path::new("qa.echo.sh")), "qa.echo");
    assert_eq!(infer_tool_name(Path::new("qa.echo.js")), "qa.echo");
    assert_eq!(infer_tool_name(Path::new("echo.py")), "echo");
    assert_eq!(infer_tool_name(Path::new("echo.sh")), "echo");
}

#[test]
fn sidecar_manifest_path_preserves_dotted_names() {
    use super::super::manifest::sidecar_manifest_path;
    use std::path::Path;

    assert_eq!(
        sidecar_manifest_path(Path::new("qa.echo")),
        Path::new("qa.echo.orbit-tool.yaml")
    );
    assert_eq!(
        sidecar_manifest_path(Path::new("/tools/qa.echo")),
        Path::new("/tools/qa.echo.orbit-tool.yaml")
    );
    assert_eq!(
        sidecar_manifest_path(Path::new("qa.echo.py")),
        Path::new("qa.echo.orbit-tool.yaml")
    );
    assert_eq!(
        sidecar_manifest_path(Path::new("echo.py")),
        Path::new("echo.orbit-tool.yaml")
    );
}

#[test]
fn tool_scaffold_registers_full_dotted_name_in_manifest() {
    use crate::command::Execute;
    use crate::command::tool::manifest::load_external_tool_manifest;
    use crate::command::tool::scaffold::ToolScaffoldArgs;

    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("qa.echo");

    let args = ToolScaffoldArgs {
        path: script_path.display().to_string(),
        name: None,
        description: "Test description".to_string(),
        force: false,
    };

    let runtime = orbit_core::OrbitRuntime::from_roots(dir.path(), dir.path()).expect("runtime");
    args.execute(&runtime).expect("scaffold succeeds");

    assert!(script_path.exists(), "script was created");
    let manifest_path = dir.path().join("qa.echo.orbit-tool.yaml");
    assert!(
        manifest_path.exists(),
        "manifest with full dotted name was created"
    );

    let manifest = load_external_tool_manifest(&manifest_path).expect("load manifest");
    assert_eq!(
        manifest.name, "qa.echo",
        "registered name must be full name"
    );
}
