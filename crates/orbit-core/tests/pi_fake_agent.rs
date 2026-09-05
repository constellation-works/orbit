#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-11296] Deterministic end-to-end coverage for the Pi executor. The fake
// binary exercises Orbit's real runtime, runner, JSONL adapter, and shipped
// executor asset without network access or credentials.
//
// Contract verified against `@earendil-works/pi-coding-agent` 0.85.1
// (`packages/coding-agent`): the README option tables and `src/cli/args.ts` for
// the flag spellings, `src/modes/print-mode.ts` for JSON-mode stdout, and
// `docs/json.md` for the event stream this fake reproduces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::identity::ReasoningEffort;
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const PROMPT_SECRET: &str = "pi-tenant-42-authorization-bearer-zzz";
const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;
const MODEL: &str = "sonnet";

fn fake_pi(dir: &Path, body: &str, argv_path: &Path, stdin_path: &Path) -> PathBuf {
    let program = dir.join("pi");
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
    std::fs::write(&program, script).expect("write fake pi");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake pi");
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
        let program = fake_pi(dir.path(), &body, &argv_path, &stdin_path);

        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        seed_pi_executor(&runtime, &program, false);
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

fn pi_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../assets/executors/pi.yaml"))
        .expect("parse embedded Pi executor")
}

fn seed_pi_executor(runtime: &OrbitRuntime, program: &Path, keep_sandbox: bool) {
    let resource = pi_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "pi");
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
    runtime.upsert_executor_def(&def).expect("seed Pi executor");
}

fn spec(timeout_seconds: u64) -> AgentLoopSpec {
    AgentLoopSpec {
        instruction: "Return the requested Orbit response envelope.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some(MODEL.to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
        max_iterations: 1,
        backend: None,
        provider: Provider::Pi,
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
        "pi-fake",
        format!("pi:{MODEL}"),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "pi_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "pi-fake",
        host: Some(&harness.runtime),
    })
    .map_err(|error| error.to_string())
}

fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    try_dispatch(harness, spec).expect("dispatch Pi CLI backend")
}

/// The documented `--mode json` stream: a session header, lifecycle frames, the
/// authoritative `message_end`, and an `agent_end` that replays the whole
/// conversation — Orbit's own prompt included.
fn success_body() -> String {
    format!(
        r#"printf '%s\n' '{{"type":"session","version":3,"id":"9a1","timestamp":"2026-09-05T18:00:00Z","cwd":"/work"}}'
printf '%s\n' '{{"type":"agent_start"}}'
printf '%s\n' '{{"type":"turn_start"}}'
printf '%s\n' '{{"type":"message_update","usage":{{"input":12,"output":1,"cacheRead":0,"cacheWrite":0}},"assistantMessageEvent":{{"type":"text_delta","contentIndex":0,"delta":"..."}}}}'
printf '%s\n' '{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"text","text":"{SUCCESS_ENVELOPE}"}}],"provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":12,"output":34,"cacheRead":0,"cacheWrite":0}},"stopReason":"stop","timestamp":1762000000000}}}}'
printf '%s\n' '{{"type":"agent_end","messages":[{{"role":"user","content":[{{"type":"text","text":"orbit prompt echo"}}],"timestamp":1762000000000}}]}}'
exit 0"#
    )
}

#[test]
fn command_construction_matches_the_shipped_headless_contract() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let argv = harness.argv();
    assert!(argv.windows(2).any(|args| args == ["--mode", "json"]));
    assert!(argv.windows(1).any(|args| args == ["--no-session"]));
    assert!(argv.windows(1).any(|args| args == ["--no-approve"]));
    assert!(argv.windows(1).any(|args| args == ["--offline"]));
    assert!(argv.windows(2).any(|args| args == ["--model", MODEL]));
    assert!(argv.windows(2).any(|args| args == ["--thinking", "high"]));
    // Pi's own `--provider` names the underlying model vendor; Orbit never
    // renders it, so the executor lane identity cannot be re-pointed by a crew.
    assert!(!argv.iter().any(|arg| arg == "--provider"));
    assert!(!argv.iter().any(|arg| arg == "--api-key"));
    assert!(
        !argv.iter().any(|arg| arg.contains(PROMPT_SECRET)),
        "prompt must not enter argv",
    );
}

#[test]
fn prompt_is_delivered_on_stdin_and_model_identity_stays_pi() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert!(harness.stdin().contains(PROMPT_SECRET));
    assert!(harness.stdin().contains("Execution envelope:"));

    let invocation = outcome.invocation.expect("invocation trace");
    assert_eq!(invocation.provider, "pi");
    assert_eq!(invocation.model.as_deref(), Some(MODEL));
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(!rendered.contains(PROMPT_SECRET));
}

#[test]
fn successful_run_persists_worktree_edit_and_projects_result() {
    let body = format!(
        "printf 'edited by pi\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit persists"),
        "edited by pi\n"
    );
}

#[test]
fn crew_effort_is_optional_and_omitted_when_unset() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(
        &harness,
        AgentLoopSpec {
            reasoning_effort: None,
            ..spec(60)
        },
    );

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert!(!harness.argv().iter().any(|arg| arg == "--thinking"));
}

#[test]
fn non_zero_exit_fails_even_with_a_success_frame() {
    let body = success_body().replace("exit 0", "exit 9");
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(!outcome.success);
    assert!(
        outcome.message.unwrap_or_default().contains('9'),
        "the terminal exit code reaches the operator diagnostic",
    );
}

#[test]
fn later_failed_assistant_terminal_frame_cannot_report_success() {
    let body = format!(
        "{}\nprintf '%s\\n' '{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{SUCCESS_ENVELOPE}\"}}],\"stopReason\":\"aborted\"}}}}'\nexit 0",
        success_body().replace("exit 0", ""),
    );
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(
        !outcome.success,
        "a later aborted assistant terminal frame must invalidate prior completion evidence",
    );
}

#[test]
fn malformed_or_incomplete_output_never_succeeds() {
    for body in [
        // Not JSONL at all.
        "printf '%s\\n' 'not json'\nexit 0".to_string(),
        // Truncated frame.
        "printf '%s\\n' '{\"type\":\"message_end\",\"message\":'\nexit 0".to_string(),
        // A failed assistant turn that still carries envelope-shaped text.
        format!(
            "printf '%s\\n' '{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{SUCCESS_ENVELOPE}\"}}],\"stopReason\":\"aborted\"}}}}'\nexit 0"
        ),
        // Only the replayed conversation history: Orbit's own prompt must not
        // be read back as the agent's completion evidence.
        format!(
            "printf '%s\\n' '{{\"type\":\"agent_end\",\"messages\":[{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{SUCCESS_ENVELOPE}\"}}]}}]}}'\nexit 0"
        ),
        // Streaming deltas without a terminal frame: the agent stopped mid-turn.
        "printf '%s\\n' '{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"contentIndex\":0,\"delta\":\"partial\"}}'\nexit 0".to_string(),
    ] {
        let outcome = dispatch(&Harness::new(&body), spec(60));
        assert!(!outcome.success, "invalid Pi output must fail: {body}");
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
    let resource = pi_resource();
    assert!(
        resource.spec.sandbox.is_some(),
        "Pi asset must opt into the OS sandbox"
    );
    assert_eq!(resource.spec.command.as_deref(), Some("pi"));

    let dir = tempfile::tempdir().expect("missing-binary tempdir");
    let argv = dir.path().join("argv.txt");
    let stdin = dir.path().join("stdin.txt");
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let missing = dir.path().join("missing/pi");
    seed_pi_executor(&runtime, &missing, false);
    let harness = Harness {
        _dir: dir,
        argv_path: argv,
        stdin_path: stdin,
        edit_path: PathBuf::new(),
        runtime,
    };
    let error = try_dispatch(&harness, spec(60)).expect_err("missing binary must fail");
    assert!(
        error.contains("pi"),
        "stable diagnostic names binary: {error}"
    );
    assert!(
        error.contains("failed to spawn"),
        "stable diagnostic names spawn failure: {error}"
    );
}
