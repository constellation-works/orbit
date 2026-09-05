#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-11295] Deterministic end-to-end coverage for the OpenCode executor. The
// fake binary exercises Orbit's real runtime, runner, NDJSON adapter, and
// shipped executor asset without network access or credentials.
//
// Contract verified against opencode 1.18.29 (`packages/opencode`): the
// published CLI reference at https://opencode.ai/docs/cli/ for the flag
// spellings, `src/cli/cmd/run.ts` for stdin prompt handling
// (`process.stdin.isTTY ? undefined : await Bun.stdin.text()` fed through
// `resolveRunInput`), the `--format json` event emitter, and the exit-code
// paths this fake reproduces.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity};
use orbit_types::identity::ReasoningEffort;
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const PROMPT_SECRET: &str = "opencode-tenant-42-authorization-bearer-zzz";
const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;
const MODEL: &str = "anthropic/claude-sonnet-4-5";

fn fake_opencode(
    dir: &Path,
    body: &str,
    argv_path: &Path,
    stdin_path: &Path,
    cwd_path: &Path,
) -> PathBuf {
    let program = dir.join("opencode");
    let script = format!(
        r#"#!/bin/sh
: > '{argv}'
for arg in "$@"; do printf '%s\n' "$arg" >> '{argv}'; done
pwd > '{cwd}'
cat > '{stdin}'
{body}
"#,
        argv = argv_path.display(),
        stdin = stdin_path.display(),
        cwd = cwd_path.display(),
    );
    std::fs::write(&program, script).expect("write fake opencode");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake opencode");
    }
    program
}

struct Harness {
    _dir: tempfile::TempDir,
    argv_path: PathBuf,
    stdin_path: PathBuf,
    cwd_path: PathBuf,
    edit_path: PathBuf,
    runtime: OrbitRuntime,
}

impl Harness {
    fn new(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("harness tempdir");
        let argv_path = dir.path().join("argv.txt");
        let stdin_path = dir.path().join("stdin.txt");
        let cwd_path = dir.path().join("cwd.txt");
        let edit_path = dir.path().join("workspace/edited.txt");
        std::fs::create_dir_all(edit_path.parent().expect("workspace parent"))
            .expect("create fake workspace");
        let body = body.replace("{EDIT_PATH}", &edit_path.display().to_string());
        let program = fake_opencode(dir.path(), &body, &argv_path, &stdin_path, &cwd_path);

        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        seed_opencode_executor(&runtime, &program, false);
        Self {
            _dir: dir,
            argv_path,
            stdin_path,
            cwd_path,
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

    fn cwd(&self) -> String {
        std::fs::read_to_string(&self.cwd_path)
            .expect("fake agent recorded cwd")
            .trim()
            .to_string()
    }
}

fn opencode_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../assets/executors/opencode.yaml"))
        .expect("parse embedded OpenCode executor")
}

fn seed_opencode_executor(runtime: &OrbitRuntime, program: &Path, keep_sandbox: bool) {
    let resource = opencode_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "opencode");
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
        .expect("seed OpenCode executor");
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
        provider: Provider::Opencode,
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
        "opencode-fake",
        format!("opencode:{MODEL}"),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "opencode_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "opencode-fake",
        host: Some(&harness.runtime),
    })
    .map_err(|error| error.to_string())
}

fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    try_dispatch(harness, spec).expect("dispatch OpenCode CLI backend")
}

/// The documented `--format json` stream: step boundaries, a reasoning part and
/// a completed tool call that both replay Orbit's own prompt, then the
/// assistant's answer text.
fn success_body() -> String {
    format!(
        r#"printf '%s\n' '{{"type":"step_start","timestamp":1,"sessionID":"ses_7f3a","part":{{"id":"prt_a","type":"step-start","sessionID":"ses_7f3a"}}}}'
printf '%s\n' '{{"type":"reasoning","timestamp":2,"sessionID":"ses_7f3a","part":{{"id":"prt_b","type":"reasoning","sessionID":"ses_7f3a","text":"drafting {SUCCESS_ENVELOPE}","time":{{"start":1,"end":2}}}}}}'
printf '%s\n' '{{"type":"tool_use","timestamp":3,"sessionID":"ses_7f3a","part":{{"id":"prt_c","type":"tool","tool":"bash","sessionID":"ses_7f3a","state":{{"status":"completed","input":{{"command":"cat prompt"}},"output":"{SUCCESS_ENVELOPE}"}}}}}}'
printf '%s\n' '{{"type":"text","timestamp":4,"sessionID":"ses_7f3a","part":{{"id":"prt_d","type":"text","sessionID":"ses_7f3a","text":"{SUCCESS_ENVELOPE}","time":{{"start":3,"end":4}}}}}}'
printf '%s\n' '{{"type":"step_finish","timestamp":5,"sessionID":"ses_7f3a","part":{{"id":"prt_e","type":"step-finish","sessionID":"ses_7f3a","tokens":{{"input":120,"output":40}}}}}}'
exit 0"#
    )
}

#[test]
fn command_construction_matches_the_shipped_headless_contract() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let argv = harness.argv();
    assert_eq!(argv.first().map(String::as_str), Some("run"));
    assert!(argv.windows(2).any(|args| args == ["--format", "json"]));
    assert!(argv.iter().any(|arg| arg == "--auto"));
    assert!(argv.windows(2).any(|args| args == ["--model", MODEL]));
    assert!(argv.windows(2).any(|args| args == ["--variant", "high"]));
    // OpenCode's `--model provider/model` names the underlying model vendor;
    // Orbit never renders a separate provider flag, so the executor lane
    // identity cannot be re-pointed by a crew.
    assert!(!argv.iter().any(|arg| arg == "--provider"));
    // The prompt must never be a positional `[message..]` argument.
    assert!(
        !argv.iter().any(|arg| arg.contains(PROMPT_SECRET)),
        "prompt must not enter argv",
    );
    assert!(!argv.iter().any(|arg| arg.contains("schemaVersion")));
    // Session continuation and sharing would leak one run's context into
    // another, or publish it; neither is part of the headless contract.
    assert!(!argv.iter().any(|arg| arg == "--share"));
    assert!(!argv.iter().any(|arg| arg == "--continue" || arg == "-c"));
    assert!(!argv.iter().any(|arg| arg == "--session"));
}

#[test]
fn prompt_is_delivered_on_stdin_in_the_workspace_cwd() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert!(harness.stdin().contains(PROMPT_SECRET));
    assert!(harness.stdin().contains("Execution envelope:"));

    // The child runs in the dispatch cwd rather than needing `--dir`, which
    // would put the workspace path on argv.
    let cwd = harness.cwd();
    assert!(!cwd.is_empty(), "fake agent recorded a working directory");
    assert!(!harness.argv().iter().any(|arg| arg == "--dir"));

    let invocation = outcome.invocation.expect("invocation trace");
    assert_eq!(invocation.provider, "opencode");
    assert_eq!(invocation.model.as_deref(), Some(MODEL));
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(!rendered.contains(PROMPT_SECRET));
}

#[test]
fn successful_run_persists_worktree_edit_and_projects_result() {
    let body = format!(
        "printf 'edited by opencode\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit persists"),
        "edited by opencode\n"
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
    assert!(!harness.argv().iter().any(|arg| arg == "--variant"));
}

#[test]
fn an_unsupported_effort_fails_instead_of_being_silently_dropped() {
    // OpenCode forwards `--variant` verbatim to the selected model provider and
    // documents only `high`/`max`/`minimal`. An Orbit effort outside the
    // supported set must be refused rather than remapped or omitted.
    let harness = Harness::new(&success_body());
    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::Xhigh,
    ] {
        let error = try_dispatch(
            &harness,
            AgentLoopSpec {
                reasoning_effort: Some(effort),
                ..spec(60)
            },
        )
        .err()
        .unwrap_or_else(|| panic!("effort '{effort}' must be refused before spawn"));
        assert!(
            error.contains("unsupported"),
            "diagnostic must name the unsupported effort: {error}",
        );
    }
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
fn a_terminal_error_event_cannot_report_success() {
    // OpenCode emits `{"type":"error",...}` and sets exit code 1 when the
    // session fails. Normalization must invalidate the answer text that was
    // already streamed, independently of the exit status.
    let body = format!(
        r#"{}
printf '%s\n' '{{"type":"error","timestamp":6,"sessionID":"ses_7f3a","error":{{"name":"ProviderAuthError","data":{{"message":"no credentials"}}}}}}'
exit 0"#,
        success_body().replace("exit 0", ""),
    );
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(
        !outcome.success,
        "a terminal error event must invalidate prior completion evidence",
    );
}

#[test]
fn malformed_or_incomplete_output_never_succeeds() {
    for body in [
        // Not NDJSON at all.
        "printf '%s\\n' 'not json'\nexit 0".to_string(),
        // Truncated frame.
        "printf '%s\\n' '{\"type\":\"text\",\"part\":'\nexit 0".to_string(),
        // A `text` event whose part carries no text: a malformed frame, not an
        // empty answer.
        r#"printf '%s\n' '{"type":"text","timestamp":1,"sessionID":"s","part":{"id":"p","type":"text","sessionID":"s"}}'
exit 0"#
            .to_string(),
        // Only tool traffic replaying Orbit's own prompt: that is not the
        // agent's completion evidence.
        format!(
            r#"printf '%s\n' '{{"type":"tool_use","timestamp":1,"sessionID":"s","part":{{"id":"p","type":"tool","tool":"bash","sessionID":"s","state":{{"status":"completed","output":"{SUCCESS_ENVELOPE}"}}}}}}'
exit 0"#
        ),
        // Only reasoning: a draft envelope written while thinking aloud.
        format!(
            r#"printf '%s\n' '{{"type":"reasoning","timestamp":1,"sessionID":"s","part":{{"id":"p","type":"reasoning","sessionID":"s","text":"{SUCCESS_ENVELOPE}","time":{{"start":1,"end":2}}}}}}'
exit 0"#
        ),
        // Step boundaries without any answer: the agent stopped mid-turn.
        r#"printf '%s\n' '{"type":"step_start","timestamp":1,"sessionID":"s","part":{"id":"p","type":"step-start"}}'
exit 0"#
            .to_string(),
    ] {
        let outcome = dispatch(&Harness::new(&body), spec(60));
        assert!(!outcome.success, "invalid OpenCode output must fail: {body}");
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
    let resource = opencode_resource();
    assert!(
        resource.spec.sandbox.is_some(),
        "OpenCode asset must opt into the OS sandbox"
    );
    assert_eq!(resource.spec.command.as_deref(), Some("opencode"));

    let dir = tempfile::tempdir().expect("missing-binary tempdir");
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let missing = dir.path().join("missing/opencode");
    seed_opencode_executor(&runtime, &missing, false);
    let harness = Harness {
        argv_path: dir.path().join("argv.txt"),
        stdin_path: dir.path().join("stdin.txt"),
        cwd_path: dir.path().join("cwd.txt"),
        _dir: dir,
        edit_path: PathBuf::new(),
        runtime,
    };
    let error = try_dispatch(&harness, spec(60)).expect_err("missing binary must fail");
    assert!(
        error.contains("opencode"),
        "stable diagnostic names binary: {error}"
    );
    assert!(
        error.contains("failed to spawn"),
        "stable diagnostic names spawn failure: {error}"
    );
}
