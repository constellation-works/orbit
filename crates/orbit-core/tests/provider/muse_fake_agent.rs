#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-16577] Deterministic end-to-end coverage for the Muse Code executor.
// The fake `muse` binary exercises Orbit's real runtime, runner, prompt-file
// transport, JSONL adapter, and shipped executor asset without network access
// or credentials.
//
// Contract observed on Muse Code 1.4.10 (1.4.10-R6427.1): `muse exec --help`
// and offline `--provider echo` runs. `muse exec` does not read stdin and
// refuses `--prompt-file /dev/stdin`, so the prompt arrives through a regular
// file. With `--json`, stdout is JSONL records keyed by `payload_type`, ending
// with `run.terminal.completed` whose `payload.terminal` is the wire-open
// `TurnTerminal` (`completed` | `failed` | `cancelled`) and whose
// `payload.text` is the final answer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use orbit_engine::{
    DispatchOutcome, RuntimeHost, V2AuditWriter, V2DispatchInput, dispatch_v2_activity,
};
use orbit_types::identity::ReasoningEffort;
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, is_provider_unavailable};

const PROMPT_SECRET: &str = "muse-tenant-42-authorization-bearer-zzz";
/// The Orbit envelope as it appears inside a JSON string value.
const SUCCESS_ENVELOPE: &str =
    r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true},\"error\":null}"#;
const MODEL: &str = "muse-spark-1.3";
const FAKE_API_KEY: &str = "fixture-meta-api-key";

/// The static headless argv the shipped asset must render, in order.
const STATIC_ARGS: &[&str] = &[
    "exec",
    "--json",
    "--approval-mode",
    "never",
    "--disable-request-user-input",
    "--no-foreign-personal-context",
    "--disable-reminders",
    "--no-session-log",
];

/// Records argv, stdin, cwd, environment, and the prompt file (its bytes, path
/// and mode) before running `body`.
fn fake_muse(dir: &Path, body: &str) -> PathBuf {
    let program = dir.join("muse");
    let script = format!(
        r#"#!/bin/sh
out='{out}'
: > "$out/argv.txt"
for arg in "$@"; do printf '%s\n' "$arg" >> "$out/argv.txt"; done
pwd > "$out/cwd.txt"
env > "$out/env.txt"
cat > "$out/stdin.txt"
prompt=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--prompt-file' ]; then prompt="$2"; fi
  shift
done
if [ -n "$prompt" ]; then
  printf '%s' "$prompt" > "$out/prompt-path.txt"
  cat "$prompt" > "$out/prompt.txt"
  if stat -c %a "$prompt" > "$out/prompt-mode.txt" 2>/dev/null; then :; else stat -f %Lp "$prompt" > "$out/prompt-mode.txt"; fi
fi
{body}
"#,
        out = dir.display(),
    );
    std::fs::write(&program, script).expect("write fake muse");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake muse");
    }
    program
}

struct Harness {
    dir: tempfile::TempDir,
    edit_path: PathBuf,
    runtime: OrbitRuntime,
}

impl Harness {
    fn new(body: &str) -> Self {
        Self::with_sandbox(body, None)
    }

    fn with_sandbox(body: &str, sandbox: Option<ExecutorSandboxKind>) -> Self {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        Self::with_runtime(body, sandbox, runtime, None)
    }

    fn with_runtime(
        body: &str,
        sandbox: Option<ExecutorSandboxKind>,
        runtime: OrbitRuntime,
        dir: Option<tempfile::TempDir>,
    ) -> Self {
        let dir = dir.unwrap_or_else(|| tempfile::tempdir().expect("harness tempdir"));
        let edit_path = dir.path().join("workspace/edited.txt");
        std::fs::create_dir_all(edit_path.parent().expect("workspace parent"))
            .expect("create fake workspace");
        let body = body.replace("{EDIT_PATH}", &edit_path.display().to_string());
        let program = fake_muse(dir.path(), &body);
        seed_muse_executor(&runtime, &program, sandbox);
        Self {
            dir,
            edit_path,
            runtime,
        }
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name))
            .unwrap_or_else(|error| panic!("fake muse recorded {name}: {error}"))
    }

    fn argv(&self) -> Vec<String> {
        self.read("argv.txt").lines().map(str::to_string).collect()
    }

    fn env(&self) -> Vec<(String, String)> {
        self.read("env.txt")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn env_value(&self, name: &str) -> Option<String> {
        self.env()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }
}

fn muse_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../../assets/executors/muse.yaml"))
        .expect("parse embedded Muse executor")
}

fn seed_muse_executor(
    runtime: &OrbitRuntime,
    program: &Path,
    sandbox: Option<ExecutorSandboxKind>,
) {
    let resource = muse_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    assert_eq!(resource.metadata.name, "muse");
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    // Sandbox compilation has deterministic focused coverage. Keeping the
    // wrapper here would make transport cases depend on host bwrap/SBPL.
    def.sandbox = sandbox;
    runtime
        .upsert_executor_def(&def)
        .expect("seed Muse executor");
}

fn spec(timeout_seconds: u64) -> AgentLoopSpec {
    AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "Return the requested Orbit response envelope.".to_string(),
        steering_instruction: None,
        regression_risk_instruction: None,
        explore_instruction: None,
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some(MODEL.to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
        max_iterations: 1,
        backend: None,
        provider: Provider::Muse,
        wall_clock_timeout_seconds: timeout_seconds,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    }
}

fn try_dispatch(harness: &Harness, spec: AgentLoopSpec) -> Result<DispatchOutcome, String> {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "muse-fake",
        format!("muse:{MODEL}"),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "muse_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({
            "prompt": format!("Edit the checkout. Credential: {PROMPT_SECRET}"),
        }),
        audit,
        run_id: "muse-fake",
        host: Some(&harness.runtime),
    })
    .map_err(|error| error.to_string())
}

fn dispatch(harness: &Harness, spec: AgentLoopSpec) -> DispatchOutcome {
    try_dispatch(harness, spec).expect("dispatch Muse CLI backend")
}

/// One JSONL record line for the fake's stdout.
fn record(payload_type: &str, payload: &str) -> String {
    format!(
        r#"printf '%s\n' '{{"payload_type":"{payload_type}","run_id":"run_7f3a","payload":{payload}}}'"#
    )
}

/// A run whose final record carries `terminal`, after the records that must
/// never count as completion: the prompt echoes (each quoting a success
/// envelope, as Orbit's own prompt does) and a partial delta.
fn run_body(terminal: &str, exit_code: i32) -> String {
    [
        record(
            "run.lifecycle.started",
            &format!(r#"{{"prompt":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record(
            "turn.input.user",
            &format!(r#"{{"text":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record("run.output.delta", r#"{"text":"{\"schemaVersion\":1,"}"#),
        record(
            "run.terminal.completed",
            &format!(r#"{{"terminal":"{terminal}","text":"{SUCCESS_ENVELOPE}"}}"#),
        ),
        format!("exit {exit_code}"),
    ]
    .join("\n")
}

fn success_body() -> String {
    run_body("completed", 0)
}

/// A run ending in `terminal` whose `payload.text` is `text`, after the same
/// prompt echoes and delta `run_body` emits.
fn run_body_with_text(terminal: &str, text: &str, exit_code: i32) -> String {
    [
        record(
            "run.lifecycle.started",
            &format!(r#"{{"prompt":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record(
            "turn.input.user",
            &format!(r#"{{"text":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record("run.output.delta", r#"{"text":"{\"schemaVersion\":1,"}"#),
        record(
            "run.terminal.completed",
            &format!(r#"{{"terminal":"{terminal}","text":"{text}"}}"#),
        ),
        format!("exit {exit_code}"),
    ]
    .join("\n")
}

/// A run ending with an arbitrary terminal `payload`, after the prompt echoes.
fn run_body_with_payload(payload: &str, exit_code: i32) -> String {
    [
        record(
            "run.lifecycle.started",
            &format!(r#"{{"prompt":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record(
            "turn.input.user",
            &format!(r#"{{"text":"Required response schema: {SUCCESS_ENVELOPE}"}}"#),
        ),
        record("run.output.delta", r#"{"text":"{\"schemaVersion\":1,"}"#),
        record("run.terminal.completed", payload),
        format!("exit {exit_code}"),
    ]
    .join("\n")
}

#[test]
fn command_construction_matches_the_shipped_headless_contract() {
    let body = format!(
        "printf 'edited by muse\\n' > '{{EDIT_PATH}}'\n{}",
        success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let prompt_path = harness.read("prompt-path.txt");
    let mut expected: Vec<String> = STATIC_ARGS.iter().map(|arg| arg.to_string()).collect();
    expected.extend(
        [
            "--model",
            MODEL,
            "--reasoning-effort",
            "high",
            "--prompt-file",
            &prompt_path,
        ]
        .map(str::to_string),
    );
    // No Orbit sandbox here, so Muse keeps its own; `--trust-workspace` and
    // any positional prompt are absent.
    assert_eq!(harness.argv(), expected);
    assert_eq!(outcome.output["edited"], serde_json::Value::Bool(true));
    assert_eq!(
        std::fs::read_to_string(&harness.edit_path).expect("agent edit persists"),
        "edited by muse\n"
    );
}

#[test]
fn prompt_reaches_only_a_private_scratch_file_deleted_after_the_run() {
    let harness = Harness::new(&success_body());
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);

    let prompt = harness.read("prompt.txt");
    assert!(prompt.contains(PROMPT_SECRET));
    assert!(prompt.contains("Execution envelope:"));
    assert!(
        harness.read("stdin.txt").is_empty(),
        "the prompt must not also be piped to a CLI that does not read stdin",
    );
    assert!(
        !harness
            .argv()
            .iter()
            .any(|arg| arg.contains(PROMPT_SECRET) || arg.contains("schemaVersion")),
        "prompt must not enter argv",
    );
    assert_eq!(harness.read("prompt-mode.txt").trim(), "600");

    // The file sits in the run's scratch directory under the child's cwd, and
    // is gone once dispatch returns.
    let cwd = PathBuf::from(harness.read("cwd.txt").trim());
    let prompt_path = PathBuf::from(harness.read("prompt-path.txt"));
    assert!(prompt_path.is_absolute());
    assert_eq!(
        prompt_path
            .parent()
            .map(|dir| dir.canonicalize().expect("scratch dir")),
        Some(cwd.join(".orbit/tmp").canonicalize().expect("cwd scratch")),
    );
    assert!(!prompt_path.exists(), "prompt file must be deleted");

    let invocation = outcome.invocation.expect("invocation trace");
    assert_eq!(invocation.provider, "muse");
    assert_eq!(invocation.model.as_deref(), Some(MODEL));
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(!rendered.contains(PROMPT_SECRET));
}

/// Set credentials only on an isolated child, never in the parallel parent.
fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_MUSE_FAKE_AGENT_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated Muse fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("META_API_KEY", FAKE_API_KEY)
        // An outer process cannot re-enable what the executor pins.
        .env("MUSE_NO_AUTO_UPDATE", "0")
        .env("MUSE_LOGIN", "1")
        .current_dir(home.path());
    let output = orbit_common::test_env::run_child_test(&mut command, test_name, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_muse_state_roots_are_granted(harness: &Harness, roots: [&Path; 2]) {
    #[cfg(target_os = "linux")]
    let sandbox_kind = ExecutorSandboxKind::LinuxBwrap;
    #[cfg(target_os = "macos")]
    let sandbox_kind = ExecutorSandboxKind::MacosSandboxExec;
    seed_muse_executor(
        &harness.runtime,
        &harness.dir.path().join("muse"),
        Some(sandbox_kind),
    );
    let sandbox = harness
        .runtime
        .resolve_executor_sandbox("muse", None, Some(harness.dir.path()))
        .expect("resolve Muse sandbox")
        .expect("Muse sandbox is configured");

    #[cfg(target_os = "linux")]
    for root in roots {
        assert!(
            sandbox
                .fs_profile
                .modify
                .contains(&root.display().to_string()),
            "Muse child state root {} must be in its Linux write grants: {:?}",
            root.display(),
            sandbox.fs_profile.modify,
        );
    }

    #[cfg(target_os = "macos")]
    {
        let profile = orbit_exec::compile_macos_sandbox_profile(&sandbox.fs_profile, "muse")
            .expect("compile Muse sandbox profile");
        for root in roots {
            assert!(
                profile.contains(&root.display().to_string()),
                "Muse child state root {} must be in its macOS write grants",
                root.display(),
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn xdg_state_locations_reach_the_child_and_match_sandbox_grants() {
    const TEST: &str =
        "muse_fake_agent::xdg_state_locations_reach_the_child_and_match_sandbox_grants";
    if run_isolated_test(TEST) {
        return;
    }

    let home = PathBuf::from(std::env::var_os("HOME").expect("isolated HOME"));
    for (case, config_value, data_value) in [
        ("unset", None, None),
        ("empty", Some(String::new()), Some(String::new())),
        (
            "overridden",
            Some(home.join("xdg-config").display().to_string()),
            Some(home.join("xdg-data").display().to_string()),
        ),
    ] {
        let config_root = config_value
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("muse");
        let data_root = data_value
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("muse");
        std::fs::create_dir_all(&config_root).expect("create Muse config root");
        std::fs::create_dir_all(&data_root).expect("create Muse data root");
        std::fs::write(config_root.join("auth.json"), b"fixture auth state")
            .expect("seed existing Muse auth state");

        let _xdg = orbit_common::test_env::scoped([
            ("XDG_CONFIG_HOME", config_value.as_deref()),
            ("XDG_DATA_HOME", data_value.as_deref()),
        ]);
        let body = format!(
            "muse_config_home=\"${{XDG_CONFIG_HOME:-$HOME/.config}}\"\ntest -f \"$muse_config_home/muse/auth.json\" || exit 23\nprintf 'auth-store-visible\\n' > \"$out/auth-visible.txt\"\n{}",
            success_body(),
        );
        let harness = Harness::new(&body);
        let outcome = dispatch(&harness, spec(60));
        assert!(
            outcome.success,
            "dispatch failed with {case} XDG values: {:?}",
            outcome.message,
        );
        assert_eq!(
            harness.env_value("XDG_CONFIG_HOME"),
            config_value,
            "child config locator for {case} values",
        );
        assert_eq!(
            harness.env_value("XDG_DATA_HOME"),
            data_value,
            "child data locator for {case} values",
        );
        assert_eq!(harness.read("auth-visible.txt"), "auth-store-visible\n");

        let child_config_root = harness
            .env_value("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("muse");
        let child_data_root = harness
            .env_value("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("muse");
        assert_eq!(child_config_root, config_root, "config root for {case}");
        assert_eq!(child_data_root, data_root, "data root for {case}");
        assert_muse_state_roots_are_granted(&harness, [&child_config_root, &child_data_root]);
    }
}

/// A runtime whose `[execution.env].pass` is `pass`, rooted under `dir`.
fn runtime_with_pass(dir: &Path, pass: &str) -> OrbitRuntime {
    let global = dir.join("global");
    let workspace = dir.join("repo").join(".orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    std::fs::write(
        global.join("config.toml"),
        format!("[execution.env]\npass = {pass}\n"),
    )
    .expect("fixture config");
    OrbitRuntime::from_roots(&global, &workspace).expect("build runtime")
}

#[test]
fn meta_api_key_reaches_muse_only_through_an_explicit_pass() {
    if run_isolated_test("muse_fake_agent::meta_api_key_reaches_muse_only_through_an_explicit_pass")
    {
        return;
    }
    for (pass, admitted) in [
        (
            r#"["HOME", "PATH", "MUSE_NO_AUTO_UPDATE", "MUSE_LOGIN"]"#,
            false,
        ),
        (
            r#"["HOME", "PATH", "MUSE_NO_AUTO_UPDATE", "MUSE_LOGIN", "META_API_KEY"]"#,
            true,
        ),
    ] {
        let dir = tempfile::tempdir().expect("harness tempdir");
        let runtime = runtime_with_pass(dir.path(), pass);
        let harness = Harness::with_runtime(&success_body(), None, runtime, Some(dir));
        let outcome = dispatch(&harness, spec(60));
        assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
        assert_eq!(
            harness.env_value("META_API_KEY").as_deref(),
            admitted.then_some(FAKE_API_KEY),
            "pass {pass}",
        );
        assert!(
            !harness.argv().iter().any(|arg| arg.contains(FAKE_API_KEY)),
            "the API key never reaches argv",
        );
        assert_eq!(
            harness.env_value("MUSE_NO_AUTO_UPDATE").as_deref(),
            Some("1")
        );
        assert_eq!(harness.env_value("MUSE_LOGIN").as_deref(), Some("0"));
    }
}

#[test]
fn every_crew_effort_renders_verbatim_and_an_unset_effort_is_omitted() {
    // `high` is already pinned by command construction and the registry table.
    for effort in [ReasoningEffort::Xhigh, ReasoningEffort::Max] {
        let harness = Harness::new(&success_body());
        let outcome = dispatch(
            &harness,
            AgentLoopSpec {
                reasoning_effort: Some(effort),
                ..spec(60)
            },
        );
        assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
        assert!(
            harness
                .argv()
                .windows(2)
                .any(|args| args == ["--reasoning-effort".to_string(), effort.to_string()]),
        );
    }

    let harness = Harness::new(&success_body());
    let outcome = dispatch(
        &harness,
        AgentLoopSpec {
            reasoning_effort: None,
            model: None,
            ..spec(60)
        },
    );
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let argv = harness.argv();
    assert!(!argv.iter().any(|arg| arg == "--reasoning-effort"));
    assert!(!argv.iter().any(|arg| arg == "--model"));
}

#[test]
fn orbit_sandbox_turns_off_the_inner_muse_sandbox() {
    // `sandbox: off` takes the same neutralization path as an applied Orbit
    // sandbox without depending on host bwrap or Seatbelt.
    let harness = Harness::with_sandbox(&success_body(), Some(ExecutorSandboxKind::Off));
    let outcome = dispatch(&harness, spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let argv = harness.argv();
    assert_eq!(
        argv.iter()
            .filter(|arg| arg.as_str() == "--disable-sandbox")
            .count(),
        1
    );
    assert!(!argv.iter().any(|arg| arg == "--yolo"));
}

#[test]
fn non_zero_exit_fails_even_with_a_completed_record() {
    let outcome = dispatch(&Harness::new(&run_body("completed", 9)), spec(60));
    assert!(!outcome.success);
    assert!(
        outcome.message.unwrap_or_default().contains('9'),
        "the terminal exit code reaches the operator diagnostic",
    );
}

#[test]
fn completed_terminal_diagnostic_fields_cannot_supply_an_answer() {
    let mut accepted = Vec::new();
    for fallback in [
        format!(r#""message":"{SUCCESS_ENVELOPE}""#),
        format!(r#""error":"{SUCCESS_ENVELOPE}""#),
        format!(r#""error":{{"message":"{SUCCESS_ENVELOPE}"}}"#),
    ] {
        let payload = format!(r#"{{"terminal":"completed",{fallback}}}"#);
        let outcome = dispatch(&Harness::new(&run_body_with_payload(&payload, 0)), spec(60));
        if outcome.success || !outcome.output["edited"].is_null() {
            accepted.push(payload);
        }
    }
    assert!(
        accepted.is_empty(),
        "completed terminals must require string payload.text; diagnostic envelopes became answers: {accepted:?}",
    );
}

#[test]
fn failed_terminal_fallback_reasons_reach_the_operator_without_becoming_answers() {
    let reason = "provider stopped; diagnostic quotes";
    let quoted_reason = format!("{reason}: {SUCCESS_ENVELOPE}");
    for fallback in [
        format!(r#""message":"{quoted_reason}""#),
        format!(r#""error":"{quoted_reason}""#),
        format!(r#""error":{{"message":"{quoted_reason}"}}"#),
    ] {
        let payload = format!(r#"{{"terminal":"failed",{fallback}}}"#);
        let outcome = dispatch(&Harness::new(&run_body_with_payload(&payload, 0)), spec(60));
        assert!(!outcome.success, "failed terminal must fail: {payload}");
        assert!(
            outcome.output["edited"].is_null(),
            "a failure diagnostic cannot supply result fields: {payload}",
        );
        let message = outcome.message.unwrap_or_default();
        assert!(
            message.contains(reason),
            "fallback reason must reach the operator: {message}",
        );
    }
}

#[test]
fn a_terminal_other_than_completed_cannot_report_success() {
    // `TurnTerminal` is wire-open: anything but `completed` is a failure, even
    // when the record carries a success envelope and the CLI exits 0.
    for terminal in ["failed", "cancelled", "interrupted"] {
        let reason = format!("provider stopped with terminal {terminal}");
        let outcome = dispatch(
            &Harness::new(&run_body_with_text(terminal, &reason, 0)),
            spec(60),
        );
        assert!(!outcome.success, "terminal {terminal} must fail");
        let message = outcome.message.unwrap_or_default();
        assert!(
            message.contains(&reason),
            "terminal {terminal} reason must reach the operator message: {message}",
        );
        assert!(
            !is_provider_unavailable(None, Some(&message)),
            "terminal {terminal} without auth evidence must stay unclassified: {message}",
        );
    }
}

#[test]
fn failed_terminal_reason_reaches_the_message_without_provider_classification() {
    // The inaccessible-model failure Daniel observed: an ordinary run
    // failure carrying its reason, never a provider-unavailable outage.
    let reason = "model muse-wonder-full-1.0 does not exist or you lack access";
    let outcome = dispatch(
        &Harness::new(&run_body_with_text("failed", reason, 1)),
        spec(60),
    );
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains(reason),
        "the failed terminal's reason must reach the operator: {message}",
    );
    assert!(
        !is_provider_unavailable(None, Some(&message)),
        "an inaccessible model must not be stamped provider-unavailable: {message}",
    );
    assert!(
        !message.contains("muse login"),
        "an inaccessible model must not produce a relogin hint: {message}",
    );
}

#[test]
fn terminal_authentication_failure_is_typed_provider_unavailable() {
    // Auth evidence in the provider-owned terminal payload only:
    // `payload.text` carrying an authentication phrase, and HTTP status
    // fields on the payload, on either exit path.
    for (payload, exit_code) in [
        (
            r#"{"terminal":"failed","text":"authentication failed: login expired"}"#,
            1,
        ),
        (
            r#"{"terminal":"failed","text":"credentials rejected by upstream","status":401}"#,
            1,
        ),
        (
            r#"{"terminal":"failed","text":"credentials rejected by upstream","status_code":403}"#,
            0,
        ),
    ] {
        let outcome = dispatch(
            &Harness::new(&run_body_with_payload(payload, exit_code)),
            spec(60),
        );
        assert!(!outcome.success, "payload {payload} must fail");
        let message = outcome.message.unwrap_or_default();
        assert!(
            is_provider_unavailable(None, Some(&message)),
            "terminal auth failure must be typed provider-unavailable: {message}",
        );
        assert!(
            message.contains("provider=muse"),
            "the typed failure must name the muse provider: {message}",
        );
    }
}

#[test]
fn completed_answer_mentioning_authentication_still_succeeds_unclassified() {
    // The completed answer text is the agent's transcript, not the
    // provider writing about itself: phrases in it classify nothing.
    let body = run_body_with_payload(
        r#"{"terminal":"completed","text":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"edited\":true,\"note\":\"authentication failed for a tool; model muse-wonder-full-1.0 does not exist or you lack access\"},\"error\":null}"}"#,
        0,
    );
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let message = outcome.message.unwrap_or_default();
    assert!(
        !is_provider_unavailable(None, Some(&message)),
        "a completed answer must stay unclassified: {message}",
    );
}

#[test]
fn non_terminal_auth_phrases_never_classify_or_complete() {
    // Prompt echoes and deltas quoting authentication prose are not the
    // provider's terminal frame.
    let body = [
        record(
            "run.lifecycle.started",
            r#"{"prompt":"authentication failed"}"#,
        ),
        record("run.output.delta", r#"{"text":"authentication failed"}"#),
        record(
            "run.terminal.completed",
            &format!(r#"{{"terminal":"completed","text":"{SUCCESS_ENVELOPE}"}}"#),
        ),
        "exit 0".to_string(),
    ]
    .join("\n");
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let message = outcome.message.unwrap_or_default();
    assert!(
        !is_provider_unavailable(None, Some(&message)),
        "non-terminal records must not classify: {message}",
    );
}

#[test]
fn a_later_failed_terminal_record_overrides_an_earlier_completed_one() {
    let body = format!(
        "{}\n{}\nexit 0",
        run_body("completed", 0).replace("exit 0", ""),
        record(
            "run.terminal.completed",
            r#"{"terminal":"failed","text":"model error"}"#
        ),
    );
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("model error"),
        "the later failed terminal's reason must win: {message}",
    );
}

#[test]
fn malformed_or_incomplete_output_never_succeeds() {
    let echo_only = [
        record(
            "run.lifecycle.started",
            &format!(r#"{{"prompt":"{SUCCESS_ENVELOPE}"}}"#),
        ),
        record(
            "turn.input.user",
            &format!(r#"{{"text":"{SUCCESS_ENVELOPE}"}}"#),
        ),
        "exit 0".to_string(),
    ]
    .join("\n");
    let deltas_only = [
        record(
            "run.output.delta",
            &format!(r#"{{"text":"{SUCCESS_ENVELOPE}"}}"#),
        ),
        "exit 0".to_string(),
    ]
    .join("\n");
    let stray_line_before_success = format!("printf '%s\\n' 'not json'\n{}", success_body());
    let truncated_terminal = format!(
        r#"printf '%s\n' '{{"payload_type":"run.terminal.completed","payload":{{"terminal":"completed","text":"{SUCCESS_ENVELOPE}"'
exit 0"#
    );
    let record_without_type = format!("printf '%s\\n' '{{\"payload\":{{}}}}'\n{}", success_body());
    let terminal_without_text = [
        record("run.terminal.completed", r#"{"terminal":"completed"}"#),
        "exit 0".to_string(),
    ]
    .join("\n");
    for body in [
        // The prompt echo alone, which quotes Orbit's example envelope.
        echo_only,
        // Partial text with no terminal record.
        deltas_only,
        // No output at all.
        "exit 0".to_string(),
        // A line that is not a record poisons the stream.
        stray_line_before_success,
        truncated_terminal,
        record_without_type,
        terminal_without_text,
    ] {
        let outcome = dispatch(&Harness::new(&body), spec(60));
        assert!(!outcome.success, "invalid Muse output must fail: {body}");
    }
}

#[test]
fn wall_clock_timeout_cancels_the_agent_and_fails_the_step() {
    let harness = Harness::new("touch \"$out/timeout-ready\"\nsleep 30\nexit 0");
    let ready = harness.dir.path().join("timeout-ready");
    let clock_ready = ready.clone();
    let origin = Instant::now();
    // The deadline starts before spawn. Freeze it until the fake has finished
    // recording the prompt path, so cancellation cannot race that bookkeeping
    // on a loaded host. Only supervision time advances; teardown stays real.
    let outcome = orbit_core::test_support::with_cli_supervision_clock(
        move || {
            if clock_ready.exists() {
                origin + Duration::from_secs(2)
            } else {
                origin
            }
        },
        || dispatch(&harness, spec(1)),
    );
    assert!(ready.exists(), "fake agent must be ready before timeout");
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(message.contains("timeout") || message.contains("wall-clock"));
    let prompt_path = PathBuf::from(harness.read("prompt-path.txt"));
    assert!(!prompt_path.exists(), "prompt file must be deleted");
}

#[test]
fn shipped_executor_is_sandboxed_and_missing_binary_is_stable() {
    let resource = muse_resource();
    assert!(
        resource.spec.sandbox.is_some(),
        "Muse asset must opt into the OS sandbox"
    );
    assert_eq!(resource.spec.command.as_deref(), Some("muse"));

    let dir = tempfile::tempdir().expect("missing-binary tempdir");
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let missing = dir.path().join("missing/muse");
    seed_muse_executor(&runtime, &missing, None);
    let harness = Harness {
        dir,
        edit_path: PathBuf::new(),
        runtime,
    };
    let error = try_dispatch(&harness, spec(60)).expect_err("missing binary must fail");
    assert!(
        error.contains("muse"),
        "stable diagnostic names binary: {error}"
    );
    assert!(
        error.contains("failed to spawn"),
        "stable diagnostic names spawn failure: {error}"
    );
}
