//! Schema exposure and compatibility tests for `orbit.task.add`.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::RETIRED_TASK_ADD_INPUT_FIELDS;
use orbit_types::tool::ToolSessionContext;

use super::super::add::OrbitTaskAddTool;
use crate::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, Tool, ToolContext};

#[derive(Clone, Default)]
struct RecordingHost {
    call: Arc<Mutex<Option<RecordedCall>>>,
}

#[derive(Debug)]
struct RecordedCall {
    action: OrbitBuiltinAction,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
}

impl OrbitToolHost for RecordingHost {
    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        _reservation_owner: Option<crate::ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        *self.call.lock().expect("record call") = Some(RecordedCall {
            action,
            input,
            agent,
            model,
        });
        // Simulate success without touching disk (real YAML write exercised in
        // orbit-core integration tests). Return shape compatible with host.
        Ok(json!({ "id": "ORB-TEST", "title": "roundtrip" }))
    }

    fn task_scope(&self) -> OrbitTaskScope {
        OrbitTaskScope::default()
    }
}

fn mk_ctx(host: RecordingHost) -> ToolContext {
    ToolContext {
        cwd: None,
        allowed_tools: vec![],
        orbit_host: Some(Arc::new(host)),
        ..Default::default()
    }
}

fn capture_info<F, T>(f: F) -> (T, String)
where
    F: FnOnce() -> T,
{
    use std::io::{self, Write};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CaptureMakeWriter(Arc<Mutex<Vec<u8>>>);
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for CaptureMakeWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }

    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureMakeWriter(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::INFO)
        .with_target(true)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("utf8 logs");
    (result, logs)
}

#[test]
fn schema_exposes_only_trimmed_create_task_fields() {
    let schema = OrbitTaskAddTool.schema();

    let names: Vec<_> = schema.parameters.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "title",
            "description",
            "workspace",
            "acceptance_criteria",
            "tags",
            "required_tools",
            "context_files",
            "allow_missing_context",
            "priority",
            "complexity",
            "type",
            "relations",
            "crew",
            "orchestrator",
            "model",
        ]
    );

    let required: Vec<_> = schema
        .parameters
        .iter()
        .filter(|param| param.required)
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(required, vec!["title", "description", "complexity"]);
    let workspace = schema
        .parameters
        .iter()
        .find(|param| param.name == "workspace")
        .expect("workspace param");
    assert!(!workspace.required);

    for removed in RETIRED_TASK_ADD_INPUT_FIELDS {
        assert!(
            !names.contains(removed),
            "orbit.task.add schema must not expose retired field {removed}"
        );
    }

    let complexity = schema
        .parameters
        .iter()
        .find(|p| p.name == "complexity")
        .expect("complexity param");
    assert_eq!(complexity.param_type, "string");
    assert!(complexity.required);
    assert!(complexity.description.contains("low, medium, or hard"));
    assert!(
        !complexity
            .description
            .to_ascii_lowercase()
            .contains("optional"),
        "complexity must not be described as optional: {}",
        complexity.description
    );

    let relations = schema
        .parameters
        .iter()
        .find(|p| p.name == "relations")
        .expect("relations");
    assert_eq!(relations.param_type, "array");

    let context_files = schema
        .parameters
        .iter()
        .find(|p| p.name == "context_files")
        .expect("context_files");
    assert_eq!(context_files.param_type, "string_list");
    assert!(
        context_files.description.contains("filesystem anchor only"),
        "context_files help must document that a `symbol:` name is not verified: {}",
        context_files.description
    );
}

#[test]
fn add_call_rejects_retired_fields() {
    for field in RETIRED_TASK_ADD_INPUT_FIELDS {
        let host = RecordingHost::default();
        let ctx = mk_ctx(host.clone());
        let mut input = json!({
            "title": "Retired field must fail",
            "description": "orbit.task.add no longer strips extras",
            "workspace": "/tmp/test-ws",
            "complexity": "low",
            "model": "grok",
        });
        input
            .as_object_mut()
            .expect("object")
            .insert((*field).to_string(), json!("ignored"));

        let error = OrbitTaskAddTool
            .execute(&ctx, input)
            .expect_err("retired add fields must be refused");
        let message = error.to_string();
        assert!(
            message.contains(&format!("unknown field '{field}'")),
            "{field}: {message}"
        );
        assert!(message.contains("orbit.task.update"), "{field}: {message}");
        assert!(
            host.call.lock().expect("lock").is_none(),
            "host must not run when {field} is present"
        );
    }
}

#[test]
fn add_call_forwards_supported_fields() {
    let host = RecordingHost::default();
    let ctx = mk_ctx(host.clone());
    let input = json!({
        "title": "Supported add fields",
        "description": "Canonical create payload",
        "workspace": "/tmp/test-ws",
        "acceptance_criteria": ["MCP schema is trimmed"],
        "tags": ["mcp", "schema"],
        "context_files": ["file:crates/orbit-tools/src/builtin/orbit/task/add.rs"],
        "priority": "medium",
        "complexity": "medium",
        "type": "chore",
        "relations": [{"type": "related_to", "target": "ORB-00002"}],
        "model": "grok",
        "crew": "release-crew",
    });

    let res = OrbitTaskAddTool
        .execute(&ctx, input)
        .expect("supported add fields succeed");
    assert_eq!(res["id"], "ORB-TEST");
    assert!(res.get("ignored_fields").is_none());

    let recorded = host
        .call
        .lock()
        .expect("lock")
        .take()
        .expect("host was called");
    assert_eq!(recorded.action, OrbitBuiltinAction::TaskAdd);
    assert_eq!(recorded.model.as_deref(), Some("grok"));
    assert_eq!(recorded.input["crew"], "release-crew");
    assert_eq!(
        recorded.input["context_files"][0],
        "file:crates/orbit-tools/src/builtin/orbit/task/add.rs"
    );
}

#[test]
fn add_call_uses_session_workspace_when_input_omits_workspace() {
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = ToolSessionContext::with_workspace("/tmp/canonical-ws");
    let tool = OrbitTaskAddTool;

    tool.execute(
        &ctx,
        json!({
            "title": "Ambient workspace test",
            "description": "MCP session context supplies workspace",
            "complexity": "low",
            "model": "codex"
        }),
    )
    .expect("session workspace should satisfy workspace");

    let recorded = host
        .call
        .lock()
        .expect("lock")
        .take()
        .expect("host was called");
    assert_eq!(recorded.input["workspace"], "/tmp/canonical-ws");
}

#[test]
fn explicit_workspace_overrides_mismatched_session_workspace() {
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = ToolSessionContext::with_workspace("/tmp/session-ws");
    let tool = OrbitTaskAddTool;

    let (_res, logs) = capture_info(|| {
        tool.execute(
            &ctx,
            json!({
                "title": "Explicit workspace wins",
                "description": "The caller can override session context",
                "workspace": "/tmp/explicit-ws",
                "complexity": "low",
                "model": "codex"
            }),
        )
        .expect("explicit workspace should win")
    });

    let recorded = host
        .call
        .lock()
        .expect("lock")
        .take()
        .expect("host was called");
    assert_eq!(recorded.input["workspace"], "/tmp/explicit-ws");
    assert!(
        logs.contains("explicit workspace overrides MCP session context"),
        "mismatch should be logged at info level: {logs}"
    );
}

#[test]
fn add_call_missing_required_fields_returns_required_field_error() {
    let cases = [
        (
            "title",
            json!({
                "description": "missing title",
                "complexity": "low",
                "workspace": "/tmp/test-ws"
            }),
        ),
        (
            "description",
            json!({
                "title": "missing description",
                "complexity": "low",
                "workspace": "/tmp/test-ws"
            }),
        ),
        (
            "complexity",
            json!({
                "title": "missing complexity",
                "description": "complexity is now required",
                "workspace": "/tmp/test-ws"
            }),
        ),
    ];

    for (missing, input) in cases {
        let host = RecordingHost::default();
        let ctx = mk_ctx(host.clone());
        let err = OrbitTaskAddTool
            .execute(&ctx, input)
            .expect_err("missing required field should fail");
        match err {
            OrbitError::InvalidInput(message) => {
                assert_eq!(message, format!("missing `{missing}`"));
            }
            other => panic!("unexpected error for missing {missing}: {other}"),
        }
        assert!(
            host.call.lock().expect("lock").is_none(),
            "host must not be called when {missing} is missing"
        );
    }
}

#[test]
fn add_call_missing_workspace_without_session_context_returns_clear_error() {
    let host = RecordingHost::default();
    let ctx = mk_ctx(host.clone());
    let err = OrbitTaskAddTool
        .execute(
            &ctx,
            json!({
                "title": "missing workspace",
                "description": "missing workspace",
                "complexity": "low"
            }),
        )
        .expect_err("missing workspace and session context should fail");
    match err {
        OrbitError::InvalidInput(message) => {
            assert!(message.contains("missing `workspace`"), "{message}");
            assert!(message.contains("MCP session"), "{message}");
            assert!(
                message.contains("registered workspace name"),
                "error must state the accepted name form: {message}"
            );
            assert!(
                message.contains("ws_*"),
                "error must state that a logical ws_* id is accepted: {message}"
            );
        }
        other => panic!("unexpected error for missing workspace: {other}"),
    }
    assert!(
        host.call.lock().expect("lock").is_none(),
        "host must not be called when workspace cannot be resolved"
    );
}

#[test]
fn schema_workspace_param_documents_the_shared_selector_grammar() {
    let schema = OrbitTaskAddTool.schema();
    let workspace = schema
        .parameters
        .iter()
        .find(|param| param.name == "workspace")
        .expect("workspace param");

    assert!(
        workspace.description.contains("registered workspace name"),
        "workspace param must document the registered-name form: {}",
        workspace.description
    );
    assert!(
        workspace.description.contains("ws_*"),
        "workspace param must document logical ws_* ids: {}",
        workspace.description
    );
    assert!(
        workspace.description.contains("absolute path"),
        "workspace param must document the checkout-path form: {}",
        workspace.description
    );
    assert!(
        !workspace.description.contains("never a logical"),
        "workspace param must not forbid logical ids: {}",
        workspace.description
    );
}

/// A session configured with `orbit mcp serve --orchestrator <crew>` and,
/// like every MCP session, a bound workspace.
fn session_with_orchestrator(workspace: &str, orchestrator: &str) -> ToolSessionContext {
    ToolSessionContext {
        orchestrator: Some(orchestrator.to_string()),
        ..ToolSessionContext::with_workspace(workspace)
    }
}

fn add_input(title: &str) -> Value {
    json!({
        "title": title,
        "description": "orchestrator attribution default",
        "complexity": "low",
        "model": "codex"
    })
}

fn recorded_add(host: &RecordingHost) -> Value {
    host.call
        .lock()
        .expect("lock")
        .take()
        .expect("host was called")
        .input
}

#[test]
fn add_call_uses_session_orchestrator_when_input_omits_one() {
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = session_with_orchestrator("/tmp/canonical-ws", "hub");

    OrbitTaskAddTool
        .execute(&ctx, add_input("Session orchestrator default"))
        .expect("session orchestrator should apply");

    assert_eq!(recorded_add(&host)["orchestrator"], "hub");
}

#[test]
fn explicit_orchestrator_takes_precedence_over_the_session_default() {
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = session_with_orchestrator("/tmp/canonical-ws", "hub");

    let mut input = add_input("Explicit orchestrator wins");
    input["orchestrator"] = json!("relay");
    OrbitTaskAddTool
        .execute(&ctx, input)
        .expect("explicit orchestrator should win");

    assert_eq!(recorded_add(&host)["orchestrator"], "relay");
}

#[test]
fn add_call_without_a_session_orchestrator_sends_no_attribution() {
    // The absent-default path must be byte-identical to the behavior that
    // shipped before the session default existed, so a server started without
    // the flag cannot start attributing tasks to anyone.
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = ToolSessionContext::with_workspace("/tmp/canonical-ws");

    OrbitTaskAddTool
        .execute(&ctx, add_input("No attribution"))
        .expect("absent default should preserve existing behavior");

    assert!(
        recorded_add(&host).get("orchestrator").is_none(),
        "an unconfigured session must not invent an orchestrator"
    );
}

#[test]
fn a_blank_session_orchestrator_is_the_same_as_none() {
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = session_with_orchestrator("/tmp/canonical-ws", "   ");

    OrbitTaskAddTool
        .execute(&ctx, add_input("Blank default"))
        .expect("blank default should be ignored");

    assert!(recorded_add(&host).get("orchestrator").is_none());
}

#[test]
fn the_session_orchestrator_is_isolated_between_sessions() {
    // Two clients against the same tool must not see each other's configured
    // default: it lives on the per-session context, never in process state.
    let host = RecordingHost::default();
    let mut hub = mk_ctx(host.clone());
    hub.session_context = session_with_orchestrator("/tmp/canonical-ws", "hub");
    let mut relay = mk_ctx(host.clone());
    relay.session_context = session_with_orchestrator("/tmp/canonical-ws", "relay");
    let mut unconfigured = mk_ctx(host.clone());
    unconfigured.session_context = ToolSessionContext::with_workspace("/tmp/canonical-ws");

    for (ctx, expected) in [
        (&hub, Some("hub")),
        (&relay, Some("relay")),
        (&unconfigured, None),
        (&hub, Some("hub")),
    ] {
        OrbitTaskAddTool
            .execute(ctx, add_input("Session isolation"))
            .expect("add succeeds");
        assert_eq!(
            recorded_add(&host)
                .get("orchestrator")
                .and_then(Value::as_str),
            expected
        );
    }
}

#[test]
fn the_session_orchestrator_grants_no_capability_and_selects_no_execution_crew() {
    // Attribution is independent of authority and of execution provenance:
    // the default must not appear as a capability, a `crew`, or a `model`.
    let host = RecordingHost::default();
    let mut ctx = mk_ctx(host.clone());
    ctx.session_context = session_with_orchestrator("/tmp/canonical-ws", "hub");

    OrbitTaskAddTool
        .execute(&ctx, add_input("Independent provenance"))
        .expect("add succeeds");

    let recorded = recorded_add(&host);
    assert_eq!(recorded["orchestrator"], "hub");
    assert!(recorded.get("crew").is_none());
    assert_eq!(recorded["model"], "codex");
    assert!(
        ctx.session_context.effective_capabilities.is_empty(),
        "attribution must not add a session capability"
    );
}
