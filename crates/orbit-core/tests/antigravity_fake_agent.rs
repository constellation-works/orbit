#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-11299] Deterministic end-to-end coverage for the Antigravity executor.
// The fake binary exercises Orbit's real runtime, runner, envelope adapter,
// and shipped executor asset without network access or credentials.
// Verified CLI contract: Antigravity CLI (agy) 1.1.27. These tests are
// fixture-driven and are not an authenticated live `agy` run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const PROMPT_SECRET: &str = "agy-tenant-42-authorization-bearer-zzz";
const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;

fn fake_agy(dir: &Path, body: &str, argv_path: &Path, stdin_path: &Path) -> PathBuf {
    let program = dir.join("agy");
    let script = format!(
        r#"#!/bin/sh
: > '{argv}'
for arg in "$@"; do printf '%s\n' "$arg" >> '{argv}'; done
cat > '{stdin}'
{body}
"#,
        argv = argv_path.display(),
        stdin = stdin_path.display(),
    );
    std::fs::write(&program, script).expect("write fake agy");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake agy");
    }
    program
}

struct Harness {
    _dir: tempfile::TempDir,
    argv_path: PathBuf,
    stdin_path: PathBuf,
    edit_path: PathBuf,
    runtime: OrbitRuntime,
}

impl Harness {
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("harness tempdir");
        let argv_path = dir.path().join("argv.txt");
        let stdin_path = dir.path().join("stdin.txt");
        let edit_path = dir.path().join("workspace/edited.txt");
        std::fs::create_dir_all(edit_path.parent().expect("workspace parent"))
            .expect("create fake workspace");
        let body = body.replace("{EDIT_PATH}", &edit_path.display().to_string());
        let program = fake_agy(dir.path(), &body, &argv_path, &stdin_path);

        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        seed_antigravity_executor(&runtime, &program, false);
        Self {
            _dir: dir,
            argv_path,
            stdin_path,
            edit_path,
            runtime,
        }
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(&self.argv_path)
            .expect("fake agent recorded argv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn stdin(&self) -> String {
        std::fs::read_to_string(&self.stdin_path).expect("fake agent recorded stdin")
    }
}

fn antigravity_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../assets/executors/antigravity.yaml"))
        .expect("parse embedded Antigravity executor")
}

fn seed_antigravity_executor(runtime: &OrbitRuntime, program: &Path, keep_sandbox: bool) {
    let resource = antigravity_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "antigravity");
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    if !keep_sandbox {
        def.sandbox = None;
    }
    runtime
        .upsert_executor_def(&def)
        .expect("seed Antigravity executor");
}

fn spec(timeout_seconds: u64) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some("gemini-3.8-flash-high".to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Antigravity,
        wall_clock_timeout_seconds: timeout_seconds,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
    }
}

fn try_dispatch(harness: &Harness, spec: AgentLoopSpec) -> Result<DispatchOutcome, String> {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "agy-fake",
        "antigravity:gemini-3.8-flash-high".to_string(),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "antigravity_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "agy-fake",
        host: Some(&harness.runtime),
    })
    .map_err(|error| error.to_string())
}

fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    try_dispatch(harness, spec).expect("dispatch Antigravity CLI backend")
}

fn success_body() -> String {
    format!(
        r#"printf '%s\n' '{{"event":"init","conversation_id":"c1","init":{{"cwd":"/tmp","tools":[],"permission_mode":"always-proceed"}}}}'
printf '%s\n' '{{"event":"result","result":{{"conversation_id":"c1","status":"SUCCESS","response":"{SUCCESS_ENVELOPE}","usage":{{"input_tokens":10,"output_tokens":4,"thinking_tokens":2,"cache_read_tokens":1,"total_tokens":17}}}}}}'
exit 0"#
    )
}

#[test]
fn command_construction_matches_the_shipped_headless_contract() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let argv = harness.argv();
    assert!(
        argv.windows(2)
            .any(|args| args == ["--input-format", "stream-json"])
    );
    assert!(
        argv.windows(2)
            .any(|args| args == ["--output-format", "stream-json"])
    );
    assert!(
        argv.iter()
            .any(|arg| arg == "--dangerously-skip-permissions")
    );
    assert!(
        argv.windows(2)
            .any(|args| args == ["--model", "gemini-3.8-flash-high"])
    );
    assert!(!argv.iter().any(|arg| arg == "--approval-mode"));
    assert!(!argv.iter().any(|arg| arg == "--allowed-mcp-server-names"));
    assert!(!argv.iter().any(|arg| arg == "--sandbox"));
    assert!(!argv.iter().any(|arg| arg == "-p" || arg == "--print"));
    assert!(
        !argv.iter().any(|arg| arg.contains(PROMPT_SECRET)),
        "prompt must not enter argv",
    );
}

#[test]
fn prompt_is_a_stream_json_user_event_and_model_family_stays_gemini() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let stdin = harness.stdin();
    assert!(stdin.contains(PROMPT_SECRET));
    assert!(stdin.contains("\"event\":\"user\"") || stdin.contains("\"event\": \"user\""));
    assert!(stdin.contains("Execution envelope:"));

    let invocation = outcome.invocation.expect("invocation trace");
    assert_eq!(invocation.provider, "antigravity");
    assert_eq!(invocation.model.as_deref(), Some("gemini-3.8-flash-high"));
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(!rendered.contains(PROMPT_SECRET));
}

#[test]
fn successful_run_persists_worktree_edit_and_projects_result() {
    let body = format!(
        "printf 'edited by agy\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit persists"),
        "edited by agy\n"
    );
}

#[test]
fn error_terminal_status_never_succeeds() {
    let body = r#"printf '%s\n' '{"event":"result","result":{"status":"ERROR","response":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}","error":"authentication required"}}'
exit 0"#;
    let outcome = dispatch(&Harness::new(body), spec(60));
    assert!(!outcome.success);
}

#[test]
fn malformed_or_incomplete_output_never_succeeds() {
    for body in [
        "printf '%s\\n' 'not json'\nexit 0",
        "printf '%s\\n' '{\"event\":\"init\"}'\nexit 0",
        "printf '%s\\n' '{\"event\":\"result\",\"result\":{\"status\":\"RUNNING\"}}'\nexit 0",
    ] {
        let outcome = dispatch(&Harness::new(body), spec(60));
        assert!(
            !outcome.success,
            "invalid Antigravity output must fail: {body}"
        );
    }
}

#[test]
fn wall_clock_timeout_cancels_the_agent_and_fails_the_step() {
    let outcome = dispatch(&Harness::new("sleep 30\nexit 0"), spec(1));
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(message.contains("timeout") || message.contains("wall-clock"));
}

#[test]
fn shipped_executor_is_sandboxed_and_missing_binary_is_stable() {
    let resource = antigravity_resource();
    assert!(
        resource.spec.sandbox.is_some(),
        "Antigravity asset must opt into the OS sandbox"
    );
    assert_eq!(resource.spec.command.as_deref(), Some("agy"));

    let dir = tempfile::tempdir().expect("missing-binary tempdir");
    let argv = dir.path().join("argv.txt");
    let stdin = dir.path().join("stdin.txt");
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let missing = dir.path().join("missing/agy");
    seed_antigravity_executor(&runtime, &missing, false);
    let harness = Harness {
        _dir: dir,
        argv_path: argv,
        stdin_path: stdin,
        edit_path: PathBuf::new(),
        runtime,
    };
    let error = try_dispatch(&harness, spec(60)).expect_err("missing binary must fail");
    assert!(
        error.contains("agy"),
        "stable diagnostic names binary: {error}"
    );
    assert!(
        error.contains("failed to spawn"),
        "stable diagnostic names spawn failure: {error}"
    );
}
