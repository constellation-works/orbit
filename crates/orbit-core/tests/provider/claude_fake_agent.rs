#![allow(missing_docs)]
#![allow(clippy::expect_used)]
// [ORB-14815] Deterministic end-to-end coverage for the Claude executor's
// completion guard. The fake `claude` binary drives Orbit's real runtime,
// runner, and envelope adapter. A second `result` turn of prose after the
// envelope turn (a scheduled wake-up) must never stand in for the envelope.
// The same harness checks which credential each crew's `env` hands the
// provider process.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_core::OrbitRuntime;
use orbit_engine::{
    DispatchError, DispatchOutcome, V2AuditWriter, V2DispatchInput, dispatch_v2_activity,
};
use orbit_types::resource::{EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};

const ENVELOPE: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

/// Synthetic worker credential. The macOS guard refuses `claude` unless
/// `CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY` is on the provider
/// environment; this fixture value is not a real token [ORB-15154].
const FIXTURE_CLAUDE_WORKER_TOKEN: &str = "fixture-claude-worker-token";

/// Built-in pass names plus the synthetic credential. `execution.env.pass`
/// replaces the default list, so the fixture restates every built-in name.
const FIXTURE_PASS_CONFIG: &str = r#"[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "__CF_USER_TEXT_ENCODING", "CLAUDE_CODE_OAUTH_TOKEN"]
"#;

/// Final `result` document of a turn that ended with the envelope in
/// `structured_output`, as `claude -p --json-schema` writes it.
fn envelope_turn() -> String {
    format!(
        r#"printf '%s\n' '{{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{ENVELOPE}}}'
exit 0"#
    )
}

/// The wake-up turn: prose only, no envelope, after the envelope turn was
/// displaced. Shape copied from the `result_index` 1 capture in ORB-14750.
fn prose_only_wakeup_turn() -> &'static str {
    r#"printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"Loop check: nothing to act on, so I stopped the loop","result_index":1,"queued_turn_count":0}'
exit 0"#
}

fn fake_claude(dir: &Path, body: &str) -> PathBuf {
    let program = dir.join("claude");
    let script = format!(
        "#!/bin/sh\ncat > '{}'\n{body}\n",
        dir.join("stdin.txt").display()
    );
    std::fs::write(&program, script).expect("write fake claude");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake claude");
    }
    program
}

fn claude_resource() -> ExecutorResource {
    serde_yaml::from_str(include_str!("../../assets/executors/claude.yaml"))
        .expect("parse embedded Claude executor")
}

fn dispatch(program: &Path) -> DispatchOutcome {
    try_dispatch(program).expect("dispatch Claude CLI backend")
}

/// Set credentials only on an isolated child, never in the parallel parent.
fn run_isolated_test(function_name: &str, admit_token: bool) -> bool {
    let token = [("CLAUDE_CODE_OAUTH_TOKEN", FIXTURE_CLAUDE_WORKER_TOKEN)];
    run_isolated_test_with_env(function_name, if admit_token { &token } else { &[] })
}

/// [`run_isolated_test`] with `env` set on the child.
fn run_isolated_test_with_env(function_name: &str, env: &[(&str, &str)]) -> bool {
    const CHILD: &str = "ORBIT_TEST_CLAUDE_FAKE_AGENT_CHILD";
    let test_name = function_name
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(function_name);
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let home = tempfile::tempdir().expect("isolated Claude fixture home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .current_dir(home.path())
        .envs(env.iter().copied());
    let output = orbit_common::test_env::run_child_test(&mut command, test_name, home.path());
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

fn try_dispatch(program: &Path) -> Result<DispatchOutcome, DispatchError> {
    let root = tempfile::tempdir().expect("runtime root");
    let runtime = fixture_runtime(root.path(), FIXTURE_PASS_CONFIG, program);
    try_dispatch_spec(&runtime, &spec())
}

/// A runtime whose global `config.toml` is `config` and whose Claude
/// executor runs `program` unsandboxed.
fn fixture_runtime(root: &Path, config: &str, program: &Path) -> OrbitRuntime {
    let global = root.join("global");
    let workspace = root.join("repo").join(".orbit");
    std::fs::create_dir_all(&global).expect("global root");
    std::fs::create_dir_all(&workspace).expect("workspace root");
    std::fs::write(global.join("config.toml"), config).expect("fixture config");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let resource = claude_resource();
    assert_eq!(resource.schema_version, EXECUTOR_RESOURCE_SCHEMA_VERSION);
    let mut def = ExecutorDef::from_resource_spec(
        resource.metadata.name.clone(),
        resource.spec.clone(),
        resource.spec.created_at,
        resource.spec.updated_at,
    );
    def.command = Some(program.to_string_lossy().into_owned());
    def.sandbox = None;
    runtime
        .upsert_executor_def(&def)
        .expect("seed Claude executor");
    runtime
}

fn try_dispatch_spec(
    runtime: &OrbitRuntime,
    spec: &AgentLoopSpec,
) -> Result<DispatchOutcome, DispatchError> {
    let audit_dir = tempfile::tempdir().expect("audit tempdir");
    let audit = V2AuditWriter::with_disk_sinks(
        audit_dir.path(),
        Arc::new(orbit_store::Store::open_in_memory().expect("audit store")),
        "ws_test",
        "claude-fake",
        "claude:claude-opus-4-8".to_string(),
        None,
    )
    .expect("build audit writer");

    dispatch_v2_activity(V2DispatchInput {
        activity_name: "claude_fake_agent",
        spec: &ActivityV2Spec::AgentLoop(spec.clone()),
        fs_profile: None,
        input: serde_json::json!({"prompt": "Finish the task and return the envelope."}),
        audit,
        run_id: "claude-fake",
        host: Some(runtime),
    })
}

fn spec() -> AgentLoopSpec {
    AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "Return the requested Orbit response envelope.".to_string(),
        steering_instruction: None,
        regression_risk_instruction: None,
        explore_instruction: None,
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: Some("claude-opus-4-8".to_string()),
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Claude,
        wall_clock_timeout_seconds: 60,
        require_response_envelope: true,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
        crew_env: Vec::new(),
    }
}

/// Positive control: the same harness accepts a turn that carries the
/// envelope, so the failure below is the guard and not a broken fixture.
#[test]
fn envelope_turn_satisfies_the_completion_guard() {
    if run_isolated_test(
        std::any::type_name_of_val(&envelope_turn_satisfies_the_completion_guard),
        true,
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fake claude tempdir");
    let program = fake_claude(dir.path(), &envelope_turn());
    let outcome = dispatch(&program);

    assert!(outcome.success, "dispatch failed: {:?}", outcome.message);
    assert_eq!(
        outcome.output["completion_envelope_satisfied"],
        serde_json::Value::Bool(true)
    );
}

/// The exit-0 capture from ORB-14750 / ORB-14733: the only result is prose.
/// Exit 0 without an envelope stays a failure.
#[test]
fn exit_zero_prose_only_wakeup_result_fails_the_completion_guard() {
    if run_isolated_test(
        std::any::type_name_of_val(&exit_zero_prose_only_wakeup_result_fails_the_completion_guard),
        true,
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fake claude tempdir");
    let program = fake_claude(dir.path(), prose_only_wakeup_turn());
    let outcome = dispatch(&program);

    assert!(!outcome.success, "a prose-only result must not succeed");
    assert_eq!(outcome.output["exit_code"], serde_json::json!(0));
    assert_eq!(
        outcome.output["completion_envelope_required"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        outcome.output["completion_envelope_satisfied"],
        serde_json::Value::Bool(false)
    );
}

/// macOS `OrbitRuntime` still refuses provider `claude` when neither worker
/// credential reached the child. The guard keys on the provider, so this is
/// the same refusal a real `claude` binary hits, and the binary must not start.
#[cfg(target_os = "macos")]
#[test]
fn orbit_runtime_refuses_claude_without_a_worker_credential_before_launch() {
    if run_isolated_test(
        std::any::type_name_of_val(
            &orbit_runtime_refuses_claude_without_a_worker_credential_before_launch,
        ),
        false,
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fake claude tempdir");
    let marker = dir.path().join("launched");
    let body = format!("touch '{}'\n{}", marker.display(), envelope_turn());
    let program = fake_claude(dir.path(), &body);
    let error =
        try_dispatch(&program).expect_err("claude without a worker credential must be refused");
    let message = error.to_string();
    assert!(
        message.contains("CLAUDE_CODE_OAUTH_TOKEN") && message.contains("ANTHROPIC_API_KEY"),
        "the refusal names both credentials: {message}"
    );
    assert!(
        !marker.exists(),
        "the provider binary must not start without a worker credential"
    );
}

/// Synthetic values for the crew `env` fixture; none is a real credential.
const FIXTURE_CREDITS_KEY: &str = "fixture-credits-api-key";
const FIXTURE_SHARED_VALUE: &str = "fixture-shared-value";

/// Two Claude crews on one provider and model: `opus` uses the shared OAuth
/// token `pass` admits, `opus-credits` maps its own API key from a source no
/// CLI reads. `opus-extra` forwards one bare name; `opus-unset` maps a
/// source the launching environment does not hold.
const CREW_ENV_CONFIG: &str = r#"[execution.env]
pass = ["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER", "__CF_USER_TEXT_ENCODING", "CLAUDE_CODE_OAUTH_TOKEN"]

[workflow]
default_crew = "opus"

[crews.opus]
provider = "claude"
model = "claude-opus-4-8"

[crews.opus-credits]
provider = "claude"
model = "claude-opus-4-8"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_CREDITS_API_KEY_1"]

[crews.opus-extra]
provider = "claude"
model = "claude-opus-4-8"
env = ["EXTRA_SHARED_VAR"]

[crews.opus-unset]
provider = "claude"
model = "claude-opus-4-8"
env = ["ANTHROPIC_API_KEY=ANTHROPIC_UNSET_SOURCE"]
"#;

/// Dispatch the fixture activity as `crew`, resolved the way the engine
/// resolves an activity's crew, and return the environment the provider
/// process recorded.
fn crew_child_env(
    runtime: &OrbitRuntime,
    crew: &str,
    env_file: &Path,
) -> Result<std::collections::BTreeMap<String, String>, DispatchError> {
    let _ = std::fs::remove_file(env_file);
    let inline = spec();
    let resolved = orbit_engine::resolve_crew_settings(
        runtime,
        &inline,
        &serde_json::json!({ "crew": crew }),
        &serde_json::json!({}),
    )?
    .expect("the runtime resolves a named crew");
    let mut spec = inline;
    spec.provider = resolved.provider;
    spec.model = resolved.model;
    spec.crew_env = resolved.env;
    let outcome = try_dispatch_spec(runtime, &spec)?;
    assert!(outcome.success, "{crew}: {:?}", outcome.message);
    Ok(std::fs::read_to_string(env_file)
        .expect("the provider recorded its environment")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect())
}

/// A crew's `env` reaches only that crew's agents: `opus-credits` gets its
/// mapped API key and neither the shared OAuth token nor the source name,
/// while `opus` in the same runtime keeps the token and sees no key. A bare
/// name reaches only the crew that lists it, and a crew whose source is unset
/// is refused before its provider starts instead of falling back to the
/// shared login. On macOS the mapped key alone passes the worker-credential
/// gate, which reads the composed crew environment.
#[test]
fn crew_env_reaches_only_its_crew_and_replaces_the_shared_claude_credential() {
    if run_isolated_test_with_env(
        std::any::type_name_of_val(
            &crew_env_reaches_only_its_crew_and_replaces_the_shared_claude_credential,
        ),
        &[
            ("CLAUDE_CODE_OAUTH_TOKEN", FIXTURE_CLAUDE_WORKER_TOKEN),
            ("ANTHROPIC_CREDITS_API_KEY_1", FIXTURE_CREDITS_KEY),
            ("EXTRA_SHARED_VAR", FIXTURE_SHARED_VALUE),
        ],
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fake claude tempdir");
    let env_file = dir.path().join("env.txt");
    let body = format!("env > '{}'\n{}", env_file.display(), envelope_turn());
    let program = fake_claude(dir.path(), &body);
    let root = tempfile::tempdir().expect("runtime root");
    let runtime = fixture_runtime(root.path(), CREW_ENV_CONFIG, &program);

    let credits = crew_child_env(&runtime, "opus-credits", &env_file).expect("opus-credits runs");
    assert_eq!(
        credits.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some(FIXTURE_CREDITS_KEY),
        "the crew's agent receives the mapped key"
    );
    for absent in [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_CREDITS_API_KEY_1",
        "EXTRA_SHARED_VAR",
    ] {
        assert!(
            !credits.contains_key(absent),
            "opus-credits must not receive {absent}"
        );
    }

    let shared = crew_child_env(&runtime, "opus", &env_file).expect("opus runs");
    assert_eq!(
        shared.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some(FIXTURE_CLAUDE_WORKER_TOKEN),
        "a crew without env keeps the shared credential"
    );
    for absent in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_CREDITS_API_KEY_1",
        "EXTRA_SHARED_VAR",
    ] {
        assert!(
            !shared.contains_key(absent),
            "opus must not receive {absent}"
        );
    }

    let extra = crew_child_env(&runtime, "opus-extra", &env_file).expect("opus-extra runs");
    assert_eq!(
        extra.get("EXTRA_SHARED_VAR").map(String::as_str),
        Some(FIXTURE_SHARED_VALUE),
        "a bare name reaches the crew that lists it"
    );
    assert_eq!(
        extra.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str),
        Some(FIXTURE_CLAUDE_WORKER_TOKEN),
        "a crew that sets no Claude credential keeps the shared one"
    );

    let refused = crew_child_env(&runtime, "opus-unset", &env_file)
        .expect_err("a crew whose env source is unset must be refused")
        .to_string();
    assert!(
        refused.contains("ANTHROPIC_API_KEY (from ANTHROPIC_UNSET_SOURCE)"),
        "the refusal names the variable and its source: {refused}"
    );
    assert!(
        !env_file.exists(),
        "the provider must not start on another login when the crew's source is unset"
    );
}
