use orbit_types::task::TaskStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::json;

use super::super::test_support::{create_task, test_runtime};

/// End-to-end coverage for the artifact read surface: attach through the
/// canonical put tool, list compact metadata, then retrieve the payload.
mod artifact_get {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use orbit_types::task::TaskStatus;
    use serde_json::{Value, json};

    use super::super::super::test_support::{create_task, run_tool_as_operator, test_runtime};
    use crate::OrbitRuntime;

    fn attach(runtime: &OrbitRuntime, task_id: &str, path: &str, content: Vec<u8>) {
        let source = runtime.paths().repo_root.join(format!(
            "orbit-artifact-fixture-{}-{}",
            std::process::id(),
            path.replace('/', "_"),
        ));
        std::fs::write(&source, &content).expect("write artifact fixture");
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.put",
            json!({
                "id": task_id,
                "source_path": source.to_string_lossy(),
                "path": path,
                "model": "codex",
            }),
        )
        .expect("attach artifact");
        std::fs::remove_file(&source).ok();
    }

    fn get(runtime: &OrbitRuntime, task_id: &str, path: &str) -> Value {
        run_tool_as_operator(
            runtime,
            "orbit.task.artifact.get",
            json!({"id": task_id, "path": path}),
        )
        .expect("read artifact")
    }

    fn seeded_task(runtime: &OrbitRuntime, repo_root: &std::path::Path) -> String {
        create_task(
            runtime,
            repo_root,
            "artifact fixture",
            "holds synthetic artifacts",
            TaskStatus::InProgress,
            &[],
        )
        .id
        .to_string()
    }

    #[test]
    fn svg_stays_a_download_and_is_never_classified_as_a_viewable_image() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
        attach(&runtime, &id, "diagrams/active.svg", svg.to_vec());

        let read = get(&runtime, &id, "diagrams/active.svg");
        assert_eq!(read["media_type"], "image/svg+xml");
        assert_eq!(
            read["presentation"], "opaque",
            "SVG carries active content and must never be handed to a renderer"
        );
        // Still fully retrievable — fail-closed is about rendering, not access.
        let decoded = BASE64_STANDARD
            .decode(read["content_base64"].as_str().expect("base64 payload"))
            .expect("payload decodes");
        assert_eq!(decoded, svg.to_vec());
    }

    #[test]
    fn traversal_and_absolute_paths_are_refused_before_any_read() {
        let (_root, runtime, repo_root) = test_runtime();
        let id = seeded_task(&runtime, &repo_root);

        for path in [
            "../../etc/passwd",
            "/etc/passwd",
            "notes/../../escape.txt",
            "./notes.txt",
            r"notes\escape.txt",
        ] {
            let error = run_tool_as_operator(
                &runtime,
                "orbit.task.artifact.get",
                json!({"id": id, "path": path}),
            )
            .expect_err("traversal must be refused");
            assert!(
                matches!(error, orbit_common::OrbitError::InvalidInput(_)),
                "{path} should be rejected as invalid input, got {error}"
            );
        }
    }
}

#[test]
fn task_artifacts_retain_trusted_local_provenance_and_reject_ssh_mcp_attribution() {
    use orbit_tools::ToolContext;
    use orbit_types::policy::Role;
    use orbit_types::tool::{McpCapability, McpTransport};
    use std::collections::BTreeSet;

    let (_root, runtime, repo_root) = test_runtime();
    let runtime = runtime.with_automation_machine_identity(Some("local-runtime-box".into()));
    let task = create_task(
        &runtime,
        &repo_root,
        "artifact provenance task",
        "verifies trusted provenance retention",
        TaskStatus::InProgress,
        &[],
    );

    // 1. Local session with process identity records local process provenance.
    let local_source = repo_root.join("local-proc.txt");
    std::fs::write(&local_source, "from local process").expect("write local fixture");
    let local_process_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: Some("worker-proc-1".into()),
        process_machine_name: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        caller_machine_name: Some("untrusted-caller-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": local_source.to_string_lossy(),
                "path": "reports/local-proc.txt",
            }),
            Role::Admin,
            ToolContext {
                session_context: local_process_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local process artifact put");

    // 2. Local session without process identity falls back to trusted runtime identity.
    let runtime_source = repo_root.join("local-runtime.txt");
    std::fs::write(&runtime_source, "from local runtime").expect("write runtime fixture");
    let local_runtime_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        process_machine_id: None,
        process_machine_name: Some("worker-host-1".into()),
        caller_machine_id: Some("untrusted-caller-box".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": runtime_source.to_string_lossy(),
                "path": "reports/local-runtime.txt",
            }),
            Role::Admin,
            ToolContext {
                session_context: local_runtime_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("local runtime artifact put");

    // 3. SSH MCP session carries self-asserted caller labels and destination process id;
    // preloaded payload is accepted, but origin remains None.
    let ssh_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        transport: Some(McpTransport::SshMcp),
        process_machine_id: Some("destination-machine".into()),
        process_machine_name: Some("destination-host".into()),
        caller_machine_id: Some("remote-spoke-box".into()),
        caller_machine_name: Some("remote-spoke-host".into()),
        ..Default::default()
    };
    runtime
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "artifacts": [{
                    "path": "reports/ssh-remote.txt",
                    "media_type": "text/plain",
                    "content": "from ssh remote",
                }],
            }),
            Role::Admin,
            ToolContext {
                session_context: ssh_session,
                cwd: Some(repo_root.to_string_lossy().to_string()),
                ..ToolContext::default()
            },
        )
        .expect("ssh mcp artifact put");

    let manifest = runtime
        .get_task_artifact_manifest(&task.id)
        .expect("artifact manifest");

    let proc_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-proc.txt")
        .expect("local proc artifact");
    assert_eq!(
        proc_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "worker-proc-1".into(),
            machine_name: Some("worker-host-1".into()),
        })
    );

    let runtime_art = manifest
        .iter()
        .find(|a| a.path == "reports/local-runtime.txt")
        .expect("local runtime artifact");
    assert_eq!(
        runtime_art.origin,
        Some(orbit_types::task::ExecutionLocation {
            machine_id: "local-runtime-box".into(),
            machine_name: Some("worker-host-1".into()),
        })
    );

    let remote_art = manifest
        .iter()
        .find(|a| a.path == "reports/ssh-remote.txt")
        .expect("remote artifact");
    assert_eq!(remote_art.origin, None);
}

/// Coverage evidence is read only after its action stops, when nobody can fix
/// it, so a file that does not parse as the schema is refused at put with the
/// exact parse error and stores nothing; a well-formed file is accepted.
#[test]
fn artifact_put_refuses_malformed_coverage_evidence_with_the_parse_error() {
    use super::super::test_support::run_tool_as_operator;

    let (_root, runtime, repo_root) = test_runtime();
    let task = create_task(
        &runtime,
        &repo_root,
        "coverage evidence task",
        "receives automation coverage evidence",
        TaskStatus::InProgress,
        &[],
    );
    let put = |content: &str| {
        let source = repo_root.join("automation-coverage-fixture.json");
        std::fs::write(&source, content).expect("write coverage fixture");
        let result = run_tool_as_operator(
            &runtime,
            "orbit.task.artifact.put",
            json!({
                "id": task.id,
                "source_path": source.to_string_lossy(),
                "path": "automation-coverage.json",
                "model": "codex",
            }),
        );
        std::fs::remove_file(&source).ok();
        result
    };

    let error = put(r#"{"schema_version":1,"batch_id":{}}"#).expect_err("malformed evidence");
    let orbit_common::OrbitError::InvalidInput(message) = error else {
        panic!("expected invalid input, got {error}");
    };
    assert!(
        message.contains("invalid type: map, expected a string at line 1 column"),
        "the caller needs serde's exact error to fix the file: {message}"
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, "automation-coverage.json")
            .expect("read artifact")
            .is_none()
    );

    let revision = json!({"commit": "c1", "tree": "t1"});
    put(&json!({
        "schema_version": 1,
        "batch_id": "batch",
        "consumer": "consumer",
        "epoch": "epoch",
        "input_digest": "digest",
        "action_id": task.id,
        "attempt": 1,
        "coverage": "landed_code_review_v1",
        "from_exclusive": revision,
        "through_inclusive": revision,
        "examined_commits": ["c1"],
        "examined_deliveries": ["pr:owner/repo:1"],
        "examination_complete": true,
        "checks": [{"subject": "range", "method": "review", "observation": "examined"}],
        "findings": [],
    })
    .to_string())
    .expect("well-formed evidence is stored");
}
