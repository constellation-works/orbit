#![allow(missing_docs)]

use std::collections::HashMap;
use std::time::Duration;

use orbit_common::security::child_env::MCP_MANAGED_BINDING_ENV_VARS;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_types::workflow::activity_job::V2AuditEventKind;

use super::super::super::super::crew::{apply_resolved_settings, resolve_crew_settings};
use super::super::super::super::dispatcher::DispatchError;
use super::super::super::argv::codex_mcp_server_launch_args;
use super::super::super::tests::cli_run::CliRun;
use super::super::super::tests::test_support::{TestHost, sandbox_for_test};
use super::super::dispatch::provider_child_environment;

fn child_env_value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.as_str())
}

#[test]
fn macos_sandboxed_codex_receives_explicit_ca_overrides_from_the_parent() {
    let _environment = orbit_common::test_env::scoped([
        ("CODEX_CA_CERTIFICATE", Some("/operator/codex-ca.pem")),
        ("SSL_CERT_FILE", Some("/operator/ssl-ca.pem")),
    ]);
    let host = TestHost::with_command("codex".to_string());
    let sandbox = sandbox_for_test();

    let env = provider_child_environment(&host, "codex", Some(&sandbox), &["HOME", "PATH"]);

    assert_eq!(
        child_env_value(&env, "CODEX_CA_CERTIFICATE"),
        Some("/operator/codex-ca.pem")
    );
    assert_eq!(
        child_env_value(&env, "SSL_CERT_FILE"),
        Some("/operator/ssl-ca.pem")
    );
}

#[test]
fn codex_ca_overrides_do_not_expand_other_provider_or_linux_environments() {
    let _environment = orbit_common::test_env::scoped([
        ("CODEX_CA_CERTIFICATE", Some("/operator/codex-ca.pem")),
        ("SSL_CERT_FILE", Some("/operator/ssl-ca.pem")),
    ]);
    let host = TestHost::with_command("provider".to_string());
    let macos_sandbox = sandbox_for_test();
    let linux_sandbox = super::super::super::super::dispatcher::ResolvedSandbox {
        kind: orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap,
        ..sandbox_for_test()
    };

    for (provider, sandbox) in [
        ("claude", Some(&macos_sandbox)),
        ("codex", Some(&linux_sandbox)),
        ("codex", None),
    ] {
        let env = provider_child_environment(&host, provider, sandbox, &["HOME", "PATH"]);

        assert_eq!(child_env_value(&env, "CODEX_CA_CERTIFICATE"), None);
        assert_eq!(child_env_value(&env, "SSL_CERT_FILE"), None);
    }
}

#[test]
fn run_cli_backend_finished_audit_event_keeps_stdout_stderr_blob_refs() {
    let cli = CliRun::new()
        .success_envelope()
        .plain_stderr()
        .consume_stdin(false)
        .run_id("job-audit")
        .audit_agent("codex:gpt-5.5");
    let refresh_marker = cli.root().join("persistence-refreshed");
    let mut out = cli
        .task_context(serde_json::json!({
            "persistence_refresh_marker": refresh_marker,
        }))
        .input(serde_json::json!({
            "prompt": "do it",
            "task_id": "TAUDIT"
        }))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);
    assert!(
        refresh_marker.exists(),
        "the provider exit boundary must refresh persistence before completion audit"
    );
    let stdout = "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}\n";
    assert_eq!(outcome.output["stdout_text"], stdout);
    assert_eq!(outcome.output["stdout_text_truncated"], false);
    assert_eq!(outcome.output["stdout_text_original_bytes"], stdout.len());
    let events = out.audit.events_snapshot().expect("events snapshot");
    let finished = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationFinished {
                provider,
                exit_code,
                stdout_blob_ref,
                stderr_blob_ref,
                timed_out,
                ..
            } => Some((
                provider.as_str(),
                *exit_code,
                stdout_blob_ref.as_deref(),
                stderr_blob_ref.as_deref(),
                *timed_out,
            )),
            _ => None,
        })
        .expect("finished event");

    assert_eq!(finished.0, "codex");
    assert_eq!(finished.1, Some(0));
    assert_eq!(finished.2, Some("blob-2"));
    assert_eq!(finished.3, Some("blob-3"));
    assert!(!finished.4);
    assert_eq!(
        out.recording().blob("blob-2"),
        Some(stdout.as_bytes().to_vec())
    );
    assert_eq!(
        out.recording().blob("blob-3"),
        Some(b"plain stderr\n".to_vec())
    );
}

#[test]
fn run_cli_backend_fails_closed_when_post_provider_persistence_cannot_rebind() {
    let mut out = CliRun::new()
        .success_envelope()
        .captured_stderr()
        .consume_stdin(false)
        .run_id("job-refresh-failure")
        .audit_agent("codex:gpt-5.5")
        .task_context(serde_json::json!({
            "persistence_refresh_error": "authoritative database unavailable",
        }))
        .input(serde_json::json!({"prompt": "do it"}))
        .run();

    let error = out
        .take_result()
        .expect_err("a successful envelope cannot bypass a failed durable rebind");

    assert!(
        matches!(error, DispatchError::CliInvocationPermanent(_)),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .contains("authoritative database unavailable")
    );
    assert!(
        out.audit
            .events_snapshot()
            .expect("events")
            .iter()
            .all(|event| !matches!(&event.kind, V2AuditEventKind::CliInvocationFinished { .. })),
        "provider-finished must not be claimed through an unavailable authoritative store"
    );
    assert!(
        out.recording().blob("blob-2").is_some() && out.recording().blob("blob-3").is_some(),
        "exact stdout/stderr evidence is captured before persistence rebind"
    );
}

#[test]
fn run_cli_backend_redacts_live_env_values_in_stored_blobs() {
    let secret = "live-cli-blob-secret-value";
    let _guard = orbit_common::test_env::scoped([("ORBIT_CLI_BLOB_TEST_TOKEN", Some(secret))]);
    let mut out = CliRun::new()
        .sqlite_blobs()
        .events([format!(r#"{{"log":"stdout leak {secret}"}}"#)])
        .success_envelope()
        .stderr_events([format!("stderr leak {secret}")])
        .run_id("job-cli-blob-redaction")
        .audit_agent("codex:gpt-5.5")
        .input(serde_json::json!({"prompt": format!("provider stdin contains {secret}")}))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);
    for key in ["stdin_blob_ref", "stdout_blob_ref", "stderr_blob_ref"] {
        let blob_ref = outcome.output[key].as_str().expect("blob ref");
        let text = String::from_utf8(
            out.sqlite()
                .blob_store()
                .read(blob_ref)
                .expect("read stored blob"),
        )
        .expect("stored blob utf8");
        assert!(
            !text.contains(secret),
            "{key} should not contain raw live env value: {text}"
        );
        assert!(
            text.contains("[REDACTED_ENV]"),
            "{key} should include env redaction marker: {text}"
        );
    }
}

#[test]
fn run_cli_backend_emits_provider_pid_between_the_started_and_finished_events() {
    let mut out = CliRun::new()
        .success_envelope()
        .run_id("job-pid-audit")
        .audit_agent("codex:gpt-5.5")
        .input(serde_json::json!({ "prompt": "do it" }))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);

    let events = out.audit.events_snapshot().expect("events snapshot");
    let kinds = events
        .iter()
        .map(|event| event.kind.event_type())
        .collect::<Vec<_>>();
    let started = kinds
        .iter()
        .position(|kind| *kind == "cli.invocation.started")
        .expect("cli.invocation.started event");
    let process = kinds
        .iter()
        .position(|kind| *kind == "cli.invocation.process")
        .expect("cli.invocation.process event");
    let finished = kinds
        .iter()
        .position(|kind| *kind == "cli.invocation.finished")
        .expect("cli.invocation.finished event");
    // The ordering is the contract: the PID must be durable before the child is
    // waited on, otherwise it only ever lands after the invocation is over —
    // exactly the window in which an operator needs it.
    assert!(
        started < process && process < finished,
        "pid event must be emitted after spawn and before the exit event: {kinds:?}"
    );

    let (provider, pid) = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationProcess { provider, pid, .. } => {
                Some((provider.clone(), *pid))
            }
            _ => None,
        })
        .expect("cli.invocation.process payload");
    assert_eq!(provider, "codex");
    assert_ne!(pid, 0);
    assert_ne!(
        pid,
        std::process::id(),
        "the recorded pid must be the provider child, not the engine process"
    );
}

#[test]
fn run_cli_backend_passes_provider_config_to_codex_runtime_args() {
    let mut provider_config = HashMap::new();
    provider_config.insert("sandbox".to_string(), "danger-full-access".to_string());
    provider_config.insert("approval_policy".to_string(), "never".to_string());
    provider_config.insert(
        "writable_dirs_json".to_string(),
        r#"["/tmp/orbit-a","/tmp/orbit-b"]"#.to_string(),
    );
    let mut out = CliRun::new()
        .success_envelope()
        .run_id("job-config")
        .audit_agent("codex:gpt-5.5")
        .provider_config(provider_config)
        .input(serde_json::json!({ "prompt": "do it" }))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);
    let events = out.audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => Some(argv_redacted),
            _ => None,
        })
        .expect("cli.invocation.started event");

    assert_eq!(argv[0], out.script.display().to_string());
    let managed_override_start = argv
        .len()
        .checked_sub(2)
        .expect("managed Codex command override follows transport args");
    let managed_override_value = argv[managed_override_start + 1]
        .split_once('=')
        .expect("managed Codex command config entry")
        .1;
    let managed_binary: String =
        serde_json::from_str(managed_override_value).expect("managed Codex command JSON");
    assert_eq!(
        &argv[managed_override_start..],
        codex_mcp_server_launch_args(&managed_binary)
            .expect("encode managed Codex command")
            .as_slice(),
        "the selected managed binary override must follow the transport defaults"
    );

    let default_env_vars = serde_json::to_string(MCP_MANAGED_BINDING_ENV_VARS)
        .expect("managed MCP environment names serialize");
    let mut expected_runtime_args =
        codex_mcp_server_launch_args("orbit").expect("encode default Codex MCP command");
    expected_runtime_args.extend([
        "--config".to_string(),
        "mcp_servers.orbit.args=[\"mcp\",\"serve\"]".to_string(),
        "--config".to_string(),
        "mcp_servers.orbit.enabled=true".to_string(),
        "--config".to_string(),
        format!("mcp_servers.orbit.env_vars={default_env_vars}"),
        "--config".to_string(),
        "approval_policy=\"never\"".to_string(),
        "--sandbox".to_string(),
        "danger-full-access".to_string(),
        "--add-dir".to_string(),
        "/tmp/orbit-a".to_string(),
        "--add-dir".to_string(),
        "/tmp/orbit-b".to_string(),
    ]);
    assert_eq!(
        &argv[1..managed_override_start],
        expected_runtime_args,
        "managed Codex MCP defaults must precede provider runtime config"
    );
}

#[test]
fn run_cli_backend_passes_model_to_grok_and_captures_well_formed_stdout() {
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"pong\":\"grok-smoke\"},\"error\":null}",
        "stopReason": "EndTurn"
    })
    .to_string();
    let mut out = CliRun::new()
        .provider("grok")
        .events([grok_stdout])
        .run_id("job-grok-model")
        .audit_agent("grok:grok-build")
        .executor_args(["--output-format", "json", "--prompt-file", "/dev/stdin"])
        .model("grok-build")
        .input(serde_json::json!({"prompt": "hi"}))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);
    assert!(outcome.invocation.is_some());
    assert_eq!(outcome.output["provider"], "grok");
    assert_eq!(outcome.output["stdout_blob_ref"].as_str(), Some("blob-2"));
    assert!(
        outcome
            .output
            .get("stdout_text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| text.contains("grok-smoke")),
        "stdout preview should include the grok response"
    );

    let events = out.audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => Some(argv_redacted),
            _ => None,
        })
        .expect("cli.invocation.started event");
    let model_idx = argv
        .iter()
        .position(|arg| arg == "--model")
        .expect("grok argv should include --model");
    assert_eq!(
        argv.get(model_idx + 1).map(String::as_str),
        Some("grok-build")
    );
}

/// Crew-driven regression test for ORB-00080 AC #15: a mixed fixture crew must
/// produce `--model claude-opus-4-7` for planner and `--model gpt-5.5` for
/// implementer (identity attribution stays family; no leakage of family name
/// into the --model flag that reaches the CLI).
#[test]
fn single_crew_drives_exact_model_to_agent() {
    let input = serde_json::json!({
        "prompt": "implement",
        "crew": "single-fixture",
        "task_id": "T-crew"
    });
    let mut cli = CliRun::new()
        .success_envelope()
        .consume_stdin(false)
        .run_id("job-crew-impl")
        .audit_agent(format!("codex:{TEST_CODEX_MODEL}"))
        .input(input);
    let input = cli.input_value().clone();
    let resolved = {
        let (host, spec) = cli.host_and_spec();
        resolve_crew_settings(host, spec, &input, &input)
            .expect("crew resolution")
            .expect("fixture crew config")
    };
    assert_eq!(resolved.model.as_deref(), Some(TEST_CODEX_MODEL));
    apply_resolved_settings(cli.spec_mut(), &resolved);
    let mut out = cli.run();
    let _ = out.take_result().expect("implementer cli run");

    let events_i = out.audit.events_snapshot().expect("impl events");
    let argv_i = events_i
        .iter()
        .find_map(|e| match &e.kind {
            V2AuditEventKind::CliInvocationStarted {
                argv_redacted,
                provider,
                ..
            } => {
                assert_eq!(
                    provider, "codex",
                    "identity attribution must be codex family"
                );
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("impl started event");
    let model_idx_i = argv_i
        .iter()
        .position(|a| a == "--model")
        .expect("impl argv has --model");
    assert_eq!(
        argv_i.get(model_idx_i + 1).map(String::as_str),
        Some(TEST_CODEX_MODEL),
        "implementer --model must be exact {TEST_CODEX_MODEL}, not family"
    );
}

#[test]
fn run_cli_backend_redacts_token_shaped_argv_in_audit() {
    // [ORB-00417] A token-shaped provider-CLI flag value must be redacted in
    // the persisted run record / audit event, not recorded verbatim.
    let mut out = CliRun::new()
        .success_envelope()
        .run_id("job-argv-redaction")
        .audit_agent("codex:gpt-5.5")
        .executor_args(["--api-key", "sk-secretargvtoken1234567890abcdef"])
        .input(serde_json::json!({"prompt": "hi"}))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);

    let events = out.audit.events_snapshot().expect("audit snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("a CliInvocationStarted audit event should be present");
    let joined = argv.join(" ");
    assert!(
        !joined.contains("sk-secretargvtoken1234567890abcdef"),
        "argv leaked the token: {joined}"
    );
    assert!(
        joined.contains("[REDACTED"),
        "argv should carry a redaction placeholder: {joined}"
    );
}

#[test]
fn run_cli_backend_passes_derived_antigravity_print_timeout() {
    let mut out = CliRun::new()
        .provider("antigravity")
        .command_name("agy")
        .run_id("job-agy-print-timeout")
        .audit_agent("antigravity:gemini-3.8-flash-high")
        .timeout(Duration::from_secs(3 * 60 * 60))
        .input(serde_json::json!({"prompt": "do it"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
    assert!(!outcome.success);

    let events = out.audit.events_snapshot().expect("audit snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("started event");
    assert!(
        argv.windows(2)
            .any(|pair| pair == ["--print-timeout", "2h59m30s"]),
        "long budgets must raise --print-timeout above the 5m default: {argv:?}"
    );
    assert_eq!(
        argv.iter()
            .filter(|arg| arg.as_str() == "--print-timeout" || arg.starts_with("--print-timeout="))
            .count(),
        1
    );
}
