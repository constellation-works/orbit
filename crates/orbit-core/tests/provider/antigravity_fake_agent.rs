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
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{ExecutorDef, is_provider_unavailable};

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

    fn with_print_timeout(self, value: &str) -> Self {
        let mut executor = self
            .runtime
            .get_executor_def("antigravity")
            .expect("read executor")
            .expect("configured executor");
        executor
            .args
            .extend(["--print-timeout".to_string(), value.to_string()]);
        self.runtime
            .upsert_executor_def(&executor)
            .expect("set custom provider budget");
        self
    }

    fn stdin(&self) -> String {
        std::fs::read_to_string(&self.stdin_path).expect("fake agent recorded stdin")
    }
}

fn antigravity_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../../assets/executors/antigravity.yaml"))
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
        tool_disallow_list: None,
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
        argv.windows(2)
            .any(|args| args == ["--print-timeout", "1m30s"]),
        "60s runtime plus capped 60s admission credit minus 30s margin: {argv:?}"
    );
    assert_eq!(
        argv.iter()
            .filter(|arg| arg.as_str() == "--print-timeout" || arg.starts_with("--print-timeout="))
            .count(),
        1
    );
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
fn long_budget_is_not_capped_by_the_default_five_minute_print_timeout() {
    let body = format!(
        r#"print_timeout=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--print-timeout" ]; then print_timeout="$arg"; fi
  case "$arg" in --print-timeout=*) print_timeout="${{arg#--print-timeout=}}" ;; esac
  prev="$arg"
done
case "$print_timeout" in
  ""|5m|5m0s|300s)
    printf '%s\n' '{{"event":"result","result":{{"status":"ERROR","response":"","error":"timeout waiting for response"}}}}'
    exit 1
    ;;
esac
{success}"#,
        success = success_body()
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(3 * 60 * 60));
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    let argv = harness.argv();
    assert!(
        argv.windows(2)
            .any(|args| args == ["--print-timeout", "5h59m30s"]),
        "3h runtime plus capped admission credit must raise --print-timeout above 5m: {argv:?}"
    );
}

#[test]
fn timeout_terminal_error_with_empty_stderr_fails_without_exposing_transcript() {
    let body = format!(
        r#"printf '%s\n' '{{"event":"result","result":{{"status":"ERROR","response":"{SUCCESS_ENVELOPE}","error":"timeout waiting for response"}}}}'
exit 1"#
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));
    assert!(!outcome.success);
    assert_eq!(outcome.output["timed_out"], serde_json::Value::Bool(false));
    assert_eq!(outcome.output["exit_code"], serde_json::json!(1));
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("timeout waiting for response"),
        "empty stderr must still surface the terminal error: {message}"
    );
    assert!(!message.contains(PROMPT_SECRET));
    assert!(!message.contains("edited"));
    assert!(
        !is_provider_unavailable(None, Some(&message)),
        "a provider that answered and timed out is not unusable: {message}"
    );
    let rendered = serde_json::to_string(&outcome.output).expect("serialize output");
    assert!(
        !rendered.contains(PROMPT_SECRET),
        "prompt must not appear in durable diagnostics"
    );
}

/// [ORB-13941] The on-call failure: `agy` was not signed in on the follower.
/// Its terminal error is typed as provider-unavailable, so a pull drain
/// releases the claim and excludes the crew instead of blocking the task.
#[test]
fn authentication_terminal_error_is_typed_provider_unavailable() {
    let body = format!(
        r#"printf '%s\n' '{{"event":"result","result":{{"status":"ERROR","response":"{SUCCESS_ENVELOPE}","error":"authentication failed or timed out"}}}}'
exit 1"#
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        is_provider_unavailable(None, Some(&message)),
        "an unauthenticated provider must be typed unavailable: {message}"
    );
    assert!(
        message.contains("authentication failed or timed out"),
        "{message}"
    );
    assert!(!message.contains(PROMPT_SECRET));
}

/// Progress-only terminal, as `agy` writes when it stops at `--print-timeout`.
fn progress_only_success_body() -> &'static str {
    r#"printf '%s\n' '{"event":"init","conversation_id":"c1","init":{"cwd":"/tmp","tools":[],"permission_mode":"always-proceed"}}'
printf '%s\n' '{"event":"progress","message":"Background command still running"}'
printf '%s\n' '{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"Background command still running; waiting for it to finish.","usage":{"total_tokens":17}}}'"#
}

/// These fixtures explicitly request a 1 s print-timeout, below the activity's
/// runtime plus queue allowance, so provider exhaustion is independently reachable.
const SHORT_PRINT_TIMEOUT_SPEC_SECONDS: u64 = 31;

/// [ORB-14683] `agy` at its print-timeout exits 0 with a `SUCCESS` wrapper of
/// progress text. That is a spent provider budget, never completion.
#[test]
fn success_wrapper_without_envelope_at_print_timeout_is_reported_as_spent_budget() {
    let body = format!("sleep 2\n{}\nexit 0", progress_only_success_body());
    let harness = Harness::new(&body).with_print_timeout("1s");
    let outcome = dispatch(&harness, spec(SHORT_PRINT_TIMEOUT_SPEC_SECONDS));

    assert!(!outcome.success, "a spent budget must not succeed");
    assert!(
        harness
            .argv()
            .windows(2)
            .any(|args| args == ["--print-timeout", "1s"])
    );
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("--print-timeout of 1s"),
        "diagnostic must name the budget: {message}"
    );
    let duration_ms = outcome.output["duration_ms"].as_u64().expect("duration_ms");
    assert!(duration_ms >= 1000, "{duration_ms}");
    assert!(
        message.contains(&format!("ran {duration_ms} ms")),
        "diagnostic must name the elapsed time: {message}"
    );
    assert_eq!(outcome.output["exit_code"], serde_json::json!(0));
    assert_eq!(outcome.output["timed_out"], serde_json::Value::Bool(false));
    assert_eq!(
        outcome.output["completion_envelope_satisfied"],
        serde_json::Value::Bool(false)
    );
    assert!(
        outcome.output["final_message"]
            .as_str()
            .is_some_and(|text| text.contains("Background command still running")),
        "the bounded last message must be attached: {:?}",
        outcome.output["final_message"]
    );
    assert!(!message.contains(PROMPT_SECRET));
}

/// The budget is the provider's own limit, so the verdict does not depend on
/// the activity opting into envelope enforcement.
#[test]
fn print_timeout_without_envelope_fails_even_when_envelopes_are_not_required() {
    let body = format!("sleep 2\n{}\nexit 0", progress_only_success_body());
    let mut lenient = spec(SHORT_PRINT_TIMEOUT_SPEC_SECONDS);
    lenient.require_response_envelope = false;
    lenient.require_completion_envelope = false;
    let outcome = dispatch(&Harness::new(&body).with_print_timeout("1s"), lenient);
    assert!(!outcome.success);
    assert!(
        outcome
            .message
            .unwrap_or_default()
            .contains("--print-timeout of 1s")
    );
}

#[test]
fn success_wrapper_without_envelope_before_print_timeout_keeps_the_envelope_message() {
    let body = format!("{}\nexit 0", progress_only_success_body());
    let mut completion_only = spec(SHORT_PRINT_TIMEOUT_SPEC_SECONDS);
    completion_only.require_response_envelope = false;
    let outcome = dispatch(
        &Harness::new(&body).with_print_timeout("1s"),
        completion_only,
    );
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("agent step did not complete: the provider exited 0")
            && !message.contains("print-timeout"),
        "an early exit is the ordinary completion violation: {message}"
    );
}

#[test]
fn valid_envelope_at_print_timeout_still_succeeds() {
    let body = format!("sleep 2\n{}", success_body());
    let outcome = dispatch(
        &Harness::new(&body).with_print_timeout("1s"),
        spec(SHORT_PRINT_TIMEOUT_SPEC_SECONDS),
    );
    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert!(outcome.output["duration_ms"].as_u64().expect("duration_ms") >= 1000);
}

#[test]
fn error_terminal_after_print_timeout_keeps_the_terminal_error_message() {
    let body = r#"sleep 2
printf '%s\n' '{"event":"result","result":{"status":"ERROR","response":"","error":"timeout waiting for response"}}'
exit 1"#;
    let outcome = dispatch(
        &Harness::new(body).with_print_timeout("1s"),
        spec(SHORT_PRINT_TIMEOUT_SPEC_SECONDS),
    );
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("timeout waiting for response"),
        "{message}"
    );
    assert!(!message.contains("--print-timeout of"), "{message}");
}

/// An `agy` turn that ends right after a `manage_task` status report; `report`
/// is the task line, `tail` whatever the stream closes with.
fn background_task_body(report: &str, tail: &str) -> String {
    format!(
        r#"printf '%s\n' '{{"event":"init","conversation_id":"c1","init":{{"cwd":"/tmp","tools":[],"permission_mode":"always-proceed"}}}}'
printf '%s\n' '{{"event":"tool_result","tool":"manage_task","output":"{report}"}}'
{tail}
exit 0"#
    )
}

/// [ORB-15243] `agy` ends its headless turn with a background task it started
/// still running and exits 0 well before its print-timeout. The step fails
/// naming the task and the agent's last words, not the generic violation.
#[test]
fn exit_zero_with_a_running_background_task_names_the_task_and_last_message() {
    let tail = r#"printf '%s\n' '{"event":"result","result":{"conversation_id":"c1","status":"SUCCESS","response":"Waiting for the nextest run to finish.","usage":{"total_tokens":17}}}'"#;
    let body = format!(
        "printf 'partial\\n' > '{{EDIT_PATH}}'\n{}",
        background_task_body("task-34 running: cargo nextest run", tail)
    );
    let harness = Harness::new(&body);
    let outcome = dispatch(&harness, spec(60));

    assert!(!outcome.success, "an unfinished turn must not succeed");
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("task-34"),
        "names the running task: {message}"
    );
    assert!(
        message.contains("still running") && message.contains("exited 0"),
        "{message}"
    );
    assert!(
        message.contains("Last message: Waiting for the nextest run to finish."),
        "bounded last message must be attached: {message}"
    );
    assert!(
        !message.contains("provider exited 0 but stdout carried no valid"),
        "specific diagnostic replaces the generic text: {message}"
    );
    assert!(
        !message.contains(PROMPT_SECRET) && !message.contains("print-timeout of"),
        "{message}"
    );
    assert_eq!(outcome.output["exit_code"], serde_json::json!(0));
    assert_eq!(
        outcome.output["completion_envelope_satisfied"],
        serde_json::Value::Bool(false)
    );
}

/// The incident shape with no terminal wrapper at all: still named, with no
/// last message to attach.
#[test]
fn exit_zero_with_a_running_background_task_and_no_terminal_result_is_still_classified() {
    let body = background_task_body("task-34 running", "");
    let outcome = dispatch(&Harness::new(&body), spec(60));
    assert!(!outcome.success);
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("task-34") && !message.contains("Last message:"),
        "{message}"
    );
}

/// How `agy` words a task report varies by event shape; each still names the
/// task that is running, and a task reported finished afterwards is not named.
#[test]
fn background_task_reports_classify_across_event_shapes() {
    let sorted_keys = r#"{"event":"tool_result","name":"manage_task","result":{"status":"running","task_id":"task-7"}}"#;
    let call_then_result = r#"{"event":"tool_call","tool":"manage_task","input":"status"}
{"event":"tool_result","output":"task-9 is Running"}"#;
    let superseded = r#"{"event":"tool_result","output":"manage_task: task-3 running"}
{"event":"tool_result","output":"manage_task: task-4 running"}
{"event":"tool_result","output":"manage_task: task-3 completed (exit 0)"}"#;
    for (stream, running, finished) in [
        (sorted_keys, "task-7", None),
        (call_then_result, "task-9", None),
        (superseded, "task-4", Some("task-3")),
    ] {
        let body = format!("cat <<'EOF'\n{stream}\nEOF\nexit 0");
        let outcome = dispatch(&Harness::new(&body), spec(60));
        assert!(!outcome.success, "{stream}");
        let message = outcome.message.unwrap_or_default();
        assert!(
            message.contains(&format!("background task {running} was still running")),
            "{stream}: {message}"
        );
        if let Some(finished) = finished {
            assert!(!message.contains(finished), "{stream}: {message}");
        }
    }
}

#[test]
fn exit_zero_after_background_tasks_finished_keeps_the_envelope_message() {
    let body = background_task_body("task-34 completed (exit 0)", "");
    let mut completion_only = spec(60);
    completion_only.require_response_envelope = false;
    let outcome = dispatch(&Harness::new(&body), completion_only);
    assert!(
        !outcome.success,
        "exit 0 without an envelope is never completion"
    );
    let message = outcome.message.unwrap_or_default();
    assert!(
        message.contains("agent step did not complete: the provider exited 0")
            && !message.contains("background task"),
        "{message}"
    );
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
