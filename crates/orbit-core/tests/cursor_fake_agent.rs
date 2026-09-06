#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-10945] Deterministic end-to-end coverage for the Cursor executor. The
// fake binary exercises Orbit's real runtime, runner, envelope adapter, and
// shipped executor asset without network access or credentials.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const PROMPT_SECRET: &str = "cursor-tenant-42-authorization-bearer-zzz";
const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;

fn fake_cursor_agent(dir: &Path, body: &str, argv_path: &Path, stdin_path: &Path) -> PathBuf {
    let program = dir.join("cursor-agent");
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
    std::fs::write(&program, script).expect("write fake cursor-agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake cursor-agent");
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
        let program = fake_cursor_agent(dir.path(), &body, &argv_path, &stdin_path);

        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        seed_cursor_executor(&runtime, &program, false);
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

fn cursor_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../assets/executors/cursor.yaml"))
        .expect("parse embedded Cursor executor")
}

fn seed_cursor_executor(runtime: &OrbitRuntime, program: &Path, keep_sandbox: bool) {
    let resource = cursor_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "cursor");
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    if !keep_sandbox {
        // Sandbox compilation has deterministic focused coverage. Keeping the
        // wrapper here would make transport cases depend on host bwrap/SBPL.
        def.sandbox = None;
    }
    runtime
        .upsert_executor_def(&def)
        .expect("seed Cursor executor");
}

fn spec(timeout_seconds: u64) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some("gpt-5".to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Cursor,
        wall_clock_timeout_seconds: timeout_seconds,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    }
}

fn try_dispatch(harness: &Harness, spec: AgentLoopSpec) -> Result<DispatchOutcome, String> {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "cursor-fake",
        "cursor:gpt-5".to_string(),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "cursor_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "cursor-fake",
        host: Some(&harness.runtime),
    })
    .map_err(|error| error.to_string())
}

fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    try_dispatch(harness, spec).expect("dispatch Cursor CLI backend")
}

fn success_body() -> String {
    format!(
        r#"printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"duration_ms":1234,"duration_api_ms":987,"result":"{SUCCESS_ENVELOPE}","session_id":"s1","request_id":"r1"}}'
exit 0"#
    )
}

#[test]
fn command_construction_matches_the_shipped_headless_contract() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let argv = harness.argv();
    assert!(argv.windows(1).any(|args| args == ["--print"]));
    assert!(argv.windows(1).any(|args| args == ["--force"]));
    assert!(
        argv.windows(2)
            .any(|args| args == ["--output-format", "json"])
    );
    assert!(argv.windows(2).any(|args| args == ["--model", "gpt-5"]));
    assert!(
        !argv.iter().any(|arg| arg.contains(PROMPT_SECRET)),
        "prompt must not enter argv",
    );
}

#[test]
fn prompt_is_delivered_on_stdin_and_model_identity_stays_cursor() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert!(harness.stdin().contains(PROMPT_SECRET));
    assert!(harness.stdin().contains("Execution envelope:"));

    let invocation = outcome.invocation.expect("invocation trace");
    assert_eq!(invocation.provider, "cursor");
    assert_eq!(invocation.model.as_deref(), Some("gpt-5"));
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(!rendered.contains(PROMPT_SECRET));
}

#[test]
fn successful_run_persists_worktree_edit_and_projects_result() {
    let body = format!(
        "printf 'edited by cursor\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit persists"),
        "edited by cursor\n"
    );
}

#[test]
fn non_zero_exit_fails_even_with_success_json() {
    let body = success_body().replace("exit 0", "exit 9");
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(!outcome.success);
}

#[test]
fn malformed_or_incomplete_output_never_succeeds() {
    for body in [
        "printf '%s\\n' 'not json'\nexit 0",
        "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\"}'\nexit 0",
        "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":42}'\nexit 0",
    ] {
        let outcome = dispatch(&Harness::new(body), spec(60));
        assert!(!outcome.success, "invalid Cursor output must fail: {body}");
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
    let resource = cursor_resource();
    assert!(
        resource.spec.sandbox.is_some(),
        "Cursor asset must opt into the OS sandbox"
    );
    assert_eq!(resource.spec.command.as_deref(), Some("cursor-agent"));

    let dir = tempfile::tempdir().expect("missing-binary tempdir");
    let argv = dir.path().join("argv.txt");
    let stdin = dir.path().join("stdin.txt");
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let missing = dir.path().join("missing/cursor-agent");
    seed_cursor_executor(&runtime, &missing, false);
    let harness = Harness {
        _dir: dir,
        argv_path: argv,
        stdin_path: stdin,
        edit_path: PathBuf::new(),
        runtime,
    };
    let error = try_dispatch(&harness, spec(60)).expect_err("missing binary must fail");
    assert!(
        error.contains("cursor-agent"),
        "stable diagnostic names binary: {error}"
    );
    assert!(
        error.contains("failed to spawn"),
        "stable diagnostic names spawn failure: {error}"
    );
}
