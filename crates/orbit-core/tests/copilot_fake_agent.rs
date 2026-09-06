#![allow(missing_docs)]
// [ORB-10946] Deterministic end-to-end coverage for the Copilot executor. A
// fake agent stands in for the real `copilot` CLI so every case below — argv
// shape, prompt transport, exit codes, timeouts, malformed output — is exact
// and needs no network, credentials, or Copilot entitlement.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

/// A secret-looking value planted in the activity input. It must reach the
/// agent on stdin and must never appear in argv.
const PROMPT_SECRET: &str = "tenant-42-authorization-bearer-zzz";

const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;

/// Install a fake `copilot` on disk and return its path.
///
/// `body` runs after the harness has recorded argv and stdin, so every
/// scenario shares one recording contract and differs only in what the agent
/// then does. The recording paths are baked into the script rather than passed
/// through the environment, so cases stay independent under a parallel test
/// runner.
fn fake_copilot(dir: &Path, body: &str, argv_path: &Path, stdin_path: &Path) -> PathBuf {
    let program = dir.join("copilot");
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
    std::fs::write(&program, script).expect("write fake copilot");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake copilot");
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
        let edit_path = dir.path().join("workspace").join("edited.txt");
        std::fs::create_dir_all(edit_path.parent().expect("workspace parent"))
            .expect("create fake workspace");
        let body = body.replace("{EDIT_PATH}", &edit_path.display().to_string());
        let program = fake_copilot(dir.path(), &body, &argv_path, &stdin_path);

        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        seed_copilot_executor(&runtime, &program);
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
            .expect("fake agent must have recorded argv")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn stdin(&self) -> String {
        std::fs::read_to_string(&self.stdin_path).expect("fake agent must have recorded stdin")
    }
}

/// Seed the *shipped* copilot executor definition, retargeted at the fake
/// program. Everything else — the static argv, `stdout_format`, the model flag
/// — is the asset Orbit actually ships, so these tests fail if that asset
/// drifts.
fn seed_copilot_executor(runtime: &OrbitRuntime, program: &Path) {
    let resource: ExecutorResource =
        serde_yaml::from_str(include_str!("../assets/executors/copilot.yaml"))
            .expect("parse embedded copilot executor");
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "copilot");
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    // Sandbox confinement is asserted by the profile-compilation unit tests;
    // dispatching under a real OS sandbox here would make these cases depend
    // on the host having bwrap/sandbox-exec available.
    def.sandbox = None;
    runtime
        .upsert_executor_def(&def)
        .expect("seed copilot executor");
}

fn spec(model: Option<&str>, timeout_seconds: u64) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: model.map(str::to_string),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Copilot,
        wall_clock_timeout_seconds: timeout_seconds,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        trusted_host_execution: false,
    }
}

/// Dispatch through the real CLI runner. No `workspace_path`/`repo_root` pair
/// is declared: worktree pairing has its own coverage, and these cases are
/// about the Copilot lane, so the fake agent writes to absolute paths instead.
fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "copilot-fake",
        "copilot:claude-sonnet-4.5".to_string(),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "copilot_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "copilot-fake",
        host: Some(&harness.runtime),
    })
    .expect("dispatch copilot cli backend")
}

/// Emit a well-formed Copilot JSONL stream carrying the Orbit envelope,
/// preceded by the session control-plane frames a real run emits.
fn success_body() -> String {
    format!(
        r#"printf '%s\n' '{{"type":"session.created","data":{{"sessionId":"s1"}},"ephemeral":true}}'
printf '%s\n' '{{"type":"assistant.usage","data":{{"inputTokens":12,"outputTokens":3}}}}'
printf '%s\n' '{{"type":"assistant.message","data":{{"content":"{SUCCESS_ENVELOPE}"}}}}'
printf '%s\n' '{{"type":"session.idle","data":{{}},"ephemeral":true}}'
exit 0"#
    )
}

#[test]
fn command_construction_matches_the_shipped_unattended_contract() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let argv = harness.argv();

    // Unattended, non-interactive, machine-readable.
    assert!(argv.contains(&"--allow-all-tools".to_string()));
    assert!(argv.contains(&"--no-ask-user".to_string()));
    assert!(argv.contains(&"--output-format".to_string()));
    assert!(argv.contains(&"json".to_string()));
    assert!(argv.contains(&"--silent".to_string()));
    // A sandboxed run must not rewrite its own binary or leave the host.
    assert!(argv.contains(&"--no-auto-update".to_string()));
    assert!(argv.contains(&"--no-remote".to_string()));
    assert!(argv.contains(&"--no-remote-export".to_string()));

    // Permission configuration: tool auto-approval only. The blanket flags
    // also imply --allow-all-paths/--allow-all-urls, which would widen reach
    // past what Orbit's activity sandbox granted.
    for widening in [
        "--allow-all",
        "--yolo",
        "--allow-all-paths",
        "--allow-all-urls",
    ] {
        assert!(
            !argv.iter().any(|arg| arg == widening),
            "{widening} must not be passed; it widens beyond the Orbit sandbox",
        );
    }
}

#[test]
fn explicit_model_is_propagated_and_never_inferred_from_the_vendor() {
    for model in ["claude-sonnet-4.5", "gpt-5.4", "gemini-3.7-flash"] {
        let harness = Harness::new(&success_body());
        let outcome = dispatch(&harness, spec(Some(model), 60));
        assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

        let argv = harness.argv();
        let flag = argv
            .iter()
            .position(|arg| arg == "--model")
            .expect("--model must be passed");
        assert_eq!(argv[flag + 1], model);

        let invocation = outcome.invocation.as_ref().expect("invocation trace");
        // The provider identity stays `copilot` no matter which vendor
        // supplies the model.
        assert_eq!(invocation.provider, "copilot");
        assert_eq!(invocation.model.as_deref(), Some(model));
    }
}

#[test]
fn prompt_is_delivered_on_stdin_and_the_secret_never_reaches_argv() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let stdin = harness.stdin();
    assert!(stdin.contains(PROMPT_SECRET), "prompt must arrive on stdin");
    assert!(stdin.contains("Execution envelope:"));

    for arg in harness.argv() {
        assert!(
            !arg.contains(PROMPT_SECRET),
            "prompt content leaked into argv: {arg}",
        );
    }
    // The audit-visible preview must not carry it either.
    let rendered = serde_json::to_string(&outcome.output).expect("serialize outcome output");
    assert!(
        !rendered.contains(PROMPT_SECRET),
        "prompt secret leaked into the dispatch output",
    );
}

#[test]
fn successful_run_persists_agent_edits_and_reports_success() {
    let body = format!(
        "printf 'edited by copilot\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    // The envelope's `result` reaches the workflow output.
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit must persist"),
        "edited by copilot\n"
    );
}

#[test]
fn non_zero_exit_fails_even_with_a_success_envelope_on_stdout() {
    let body = format!(
        r#"printf '%s\n' '{{"type":"assistant.message","data":{{"content":"{SUCCESS_ENVELOPE}"}}}}'
exit 3"#
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));

    assert!(
        !outcome.success,
        "a non-zero exit must fail the step regardless of stdout",
    );
}

#[test]
fn auth_failure_shape_reports_no_completion_evidence() {
    // The real capture: session control-plane frames only, error on stderr.
    let body = r#"printf '%s\n' '{"type":"session.warning","data":{"message":"Third-party MCP servers are disabled by your organization'"'"'s Copilot policy.","warningType":"policy"},"ephemeral":true}'
printf '%s\n' 'Error: Authentication failed' >&2
exit 1"#;
    let harness = Harness::new(body);
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));

    assert!(
        !outcome.success,
        "an unauthenticated launch must not succeed"
    );
}

#[test]
fn exit_zero_without_an_assistant_message_is_not_success() {
    // The property AC4 turns on: a clean exit with no model output carries no
    // completion evidence, and Orbit must not manufacture one.
    let body = r#"printf '%s\n' '{"type":"session.created","data":{"sessionId":"s1"},"ephemeral":true}'
printf '%s\n' '{"type":"session.idle","data":{},"ephemeral":true}'
exit 0"#;
    let harness = Harness::new(body);
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));

    assert!(
        !outcome.success,
        "missing completion evidence must never read as success",
    );
}

#[test]
fn malformed_stdout_is_not_success() {
    let body = r#"printf '%s\n' 'this is not JSON'
printf '%s\n' '{"type":"assistant.message","data":{"content":"{ truncated'
exit 0"#;
    let harness = Harness::new(body);
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 60));

    assert!(
        !outcome.success,
        "malformed output must not read as success"
    );
}

#[test]
fn wall_clock_timeout_cancels_the_agent_and_fails_the_step() {
    let harness = Harness::new("sleep 30\nexit 0");
    let outcome = dispatch(&harness, spec(Some("claude-sonnet-4.5"), 1));

    assert!(!outcome.success, "a timed-out agent must not succeed");
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("timeout") || message.contains("wall-clock"),
        "timeout should be named in the failure message, got: {message}",
    );
}
