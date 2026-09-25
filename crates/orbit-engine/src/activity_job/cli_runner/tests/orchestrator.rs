#![allow(missing_docs)]

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_store::Store;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::crew::{apply_resolved_settings, resolve_crew_settings};
use super::super::super::dispatcher::DispatchError;
#[cfg(target_os = "linux")]
use super::super::super::dispatcher::ResolvedSandbox;
use super::super::super::sqlite_sink::V2SqliteSink;
use super::super::orchestrator::{provider_child_environment, resolved_activity_fs_profile_name};
use super::super::run_cli_backend;
use super::test_support::{
    RecordingSink, TestHost, sandbox_for_test, test_agent_loop_spec, test_agent_loop_spec_for,
    write_executable,
};

#[test]
fn cli_activity_fs_profile_resolver_preserves_named_profile() {
    assert_eq!(resolved_activity_fs_profile_name(None), "unrestricted");
    assert_eq!(
        resolved_activity_fs_profile_name(Some("implementer")),
        "implementer"
    );
}

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
    let linux_sandbox = super::super::super::dispatcher::ResolvedSandbox {
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
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\nprintf '%s\\n' 'plain stderr' >&2\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "job-audit",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let refresh_marker = temp.path().join("persistence-refreshed");
    let mut host = TestHost::with_command(script.display().to_string());
    host.task_context = Some(serde_json::json!({
        "persistence_refresh_marker": refresh_marker,
    }));
    let spec = test_agent_loop_spec(Duration::from_secs(5));
    let input = serde_json::json!({
        "prompt": "do it",
        "task_id": "TAUDIT"
    });

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-audit",
        audit.clone(),
        &input,
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert!(
        refresh_marker.exists(),
        "the provider exit boundary must refresh persistence before completion audit"
    );
    let stdout = "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}\n";
    assert_eq!(outcome.output["stdout_text"], stdout);
    assert_eq!(outcome.output["stdout_text_truncated"], false);
    assert_eq!(outcome.output["stdout_text_original_bytes"], stdout.len());
    let events = audit.events_snapshot().expect("events snapshot");
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
    assert_eq!(sink.blob("blob-2"), Some(stdout.as_bytes().to_vec()));
    assert_eq!(sink.blob("blob-3"), Some(b"plain stderr\n".to_vec()));
}

#[test]
fn run_cli_backend_fails_closed_when_post_provider_persistence_cannot_rebind() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\nprintf '%s\\n' 'captured stderr' >&2\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "job-refresh-failure",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let mut host = TestHost::with_command(script.display().to_string());
    host.task_context = Some(serde_json::json!({
        "persistence_refresh_error": "authoritative database unavailable",
    }));

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "job-refresh-failure",
        audit.clone(),
        &serde_json::json!({"prompt": "do it"}),
        None,
    )
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
        audit
            .events_snapshot()
            .expect("events")
            .iter()
            .all(|event| !matches!(&event.kind, V2AuditEventKind::CliInvocationFinished { .. })),
        "provider-finished must not be claimed through an unavailable authoritative store"
    );
    assert!(
        sink.blob("blob-2").is_some() && sink.blob("blob-3").is_some(),
        "exact stdout/stderr evidence is captured before persistence rebind"
    );
}

#[test]
fn run_cli_backend_redacts_secret_like_stdout_text_preview() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
printf '%s\n' '{"log":"Authorization: Bearer stdout-secret-token"}'
printf '%s\n' '{"x-api-key":"stdout-header-key"}'
printf '%s\n' '{"log":"sk-stdoutsecret123"}'
printf '%s\n' '{"api_key":"stdout-json-key"}'
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-stdout-redaction",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-stdout-redaction",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    let preview = outcome.output["stdout_text"]
        .as_str()
        .expect("stdout_text preview");
    assert!(!preview.contains("stdout-secret-token"));
    assert!(!preview.contains("stdout-header-key"));
    assert!(!preview.contains("stdout-json-key"));
    assert!(!preview.contains("sk-stdoutsecret123"));
    assert!(preview.contains("[REDACTED_AUTH]"));
    assert!(preview.contains("[REDACTED_API_KEY]"));
    assert_eq!(outcome.output["stdout_text_truncated"], false);
    assert_eq!(
        outcome.output["stdout_blob_ref"].as_str(),
        Some("blob-2"),
        "full stdout should remain available via blob ref"
    );
}

#[test]
fn run_cli_backend_redacts_live_env_values_in_stored_blobs() {
    let temp = tempdir().expect("tempdir");
    let secret = "live-cli-blob-secret-value";
    let _guard = orbit_common::test_env::scoped([("ORBIT_CLI_BLOB_TEST_TOKEN", Some(secret))]);
    let script = temp.path().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{{\"log\":\"stdout leak {secret}\"}}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\nprintf '%s\\n' 'stderr leak {secret}' >&2\n"
        ),
    );

    let loop_sink = Arc::new(V2SqliteSink::new(
        Arc::new(Store::open_in_memory().expect("open sqlite store")),
        "ws-test",
        "job-cli-blob-redaction",
        "codex:gpt-5.5",
        None,
        temp.path().join("audit").join("blobs"),
    ));
    let sink_for_writer: Arc<dyn AuditSink> = loop_sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "job-cli-blob-redaction",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-cli-blob-redaction",
        audit,
        &serde_json::json!({"prompt": format!("provider stdin contains {secret}")}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    for key in ["stdin_blob_ref", "stdout_blob_ref", "stderr_blob_ref"] {
        let blob_ref = outcome.output[key].as_str().expect("blob ref");
        let text = String::from_utf8(
            loop_sink
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
fn run_cli_backend_returns_error_when_declared_workspace_path_missing() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let missing = temp.path().join("missing-worktree");

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-missing-cwd",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "workspace_path": missing.display().to_string()
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));
    let input = serde_json::json!({
        "prompt": "do it",
        "task_id": "TMISSING"
    });

    let err = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-missing-cwd",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("missing declared workspace should fail");
    match err {
        DispatchError::CliInvocationFailed(message) => {
            assert!(
                message.contains(&missing.display().to_string()),
                "error should name missing path: {message}"
            );
        }
        other => panic!("expected CliInvocationFailed, got {other:?}"),
    }

    let events = audit.events_snapshot().expect("events snapshot");
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.kind, V2AuditEventKind::CliInvocationStarted { .. })),
        "CliInvocationStarted should not be emitted before cwd validation succeeds"
    );
}

#[test]
fn run_cli_backend_records_resolved_cwd_in_started_event() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let workspace_dir = tempdir().expect("workspace tempdir");
    let workspace = workspace_dir
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let workspace_string = workspace.display().to_string();

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-cwd-audit",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "workspace_path": workspace_string.clone()
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-cwd-audit",
        audit.clone(),
        &serde_json::json!({ "prompt": "do it", "task_id": "TCWD" }),
        None,
    )
    .expect("run succeeds");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    let cwd = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { cwd, .. } => cwd.as_deref(),
            _ => None,
        })
        .expect("cli.invocation.started cwd");
    assert_eq!(cwd, workspace_string);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires unprivileged user namespaces; bwrap cannot nest inside Orbit's sandbox"]
fn linux_bwrap_failed_invocation_names_ungranted_write_path_and_deny() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("worktree");
    let orbit = workspace.join(".orbit");
    fs::create_dir_all(&orbit).expect("denied Orbit root");
    let script = workspace.join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\ntouch \"$PWD/.orbit/ungranted\"\n",
    );
    let blocked_path = orbit.join("ungranted");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "implementer".to_string(),
        read: vec![format!("{}/**", workspace.display())],
        modify: vec![
            format!("{}/**", workspace.display()),
            format!("!{}/**", orbit.display()),
        ],
    };

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-linux-write-denial",
        "codex:test",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: Some(ResolvedSandbox {
            kind: orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap,
            fs_profile: profile,
            allow_fallback: false,
            managed_worktree: true,
            runtime_write_authority: Vec::new(),
        }),
        task_context: Some(serde_json::json!({
            "workspace_path": workspace.display().to_string()
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-linux-write-denial",
        audit,
        &serde_json::json!({"prompt": "attempt the write"}),
        None,
    )
    .expect("the invocation outcome should be classified");

    assert!(!outcome.success);
    let message = outcome.message.expect("Orbit-owned denial diagnostic");
    assert!(
        message.contains(&blocked_path.display().to_string()),
        "diagnostic must name the attempted path: {message}"
    );
    assert!(
        message.contains("denyModify rule"),
        "diagnostic must name the shadowing deny: {message}"
    );
    assert_eq!(outcome.output["sandbox_write_diagnostic"], message);
    assert!(!blocked_path.exists());
}

/// [ORB-10879] ORB-10878's exact shape: the agent hits a policy-denied write,
/// narrates it, and exits 0 without a terminating envelope. Attribution used to
/// be gated on a nonzero exit, so this run reached its operator with the model's
/// guess ("remount the filesystem") and no path or rule anywhere in the record.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires unprivileged user namespaces; bwrap cannot nest inside Orbit's sandbox"]
fn linux_bwrap_exit_zero_without_an_envelope_still_names_the_denied_write() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("worktree");
    let orbit = workspace.join(".orbit");
    fs::create_dir_all(&orbit).expect("denied Orbit root");
    let script = workspace.join("codex");
    // Exit 0 with no envelope on stdout — the provider "completed" cleanly.
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\ntouch \"$PWD/.orbit/ungranted\"\nexit 0\n",
    );
    let blocked_path = orbit.join("ungranted");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "unrestricted".to_string(),
        read: vec![format!("{}/**", workspace.display())],
        modify: vec![
            format!("{}/**", workspace.display()),
            format!("!{}/**", orbit.display()),
        ],
    };

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-linux-exit-zero-denial",
        "claude:test",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: Some(ResolvedSandbox {
            kind: orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap,
            fs_profile: profile,
            allow_fallback: false,
            managed_worktree: true,
            runtime_write_authority: Vec::new(),
        }),
        task_context: Some(serde_json::json!({
            "workspace_path": workspace.display().to_string()
        })),
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-linux-exit-zero-denial",
        audit,
        &serde_json::json!({"prompt": "attempt the write"}),
        None,
    )
    .expect("the invocation outcome should be classified");

    assert!(
        !outcome.success,
        "an agent that stopped mid-turn must not checkpoint success"
    );
    assert_eq!(outcome.output["exit_code"], serde_json::json!(0));
    let diagnostic = outcome.output["sandbox_write_diagnostic"]
        .as_str()
        .expect("exit 0 must not suppress the write-denial attribution");
    assert!(
        diagnostic.contains(&blocked_path.display().to_string()),
        "diagnostic must name the attempted path: {diagnostic}"
    );
    assert!(
        diagnostic.contains("denyModify rule"),
        "diagnostic must name the shadowing deny: {diagnostic}"
    );

    // The step message is what becomes `job_run_steps.error_message` and then
    // the task's `workflow_run_failed` note, so the attribution has to be in it.
    let message = outcome.message.expect("failed step carries a message");
    assert!(
        message.contains("did not complete"),
        "the frame classification must survive: {message}"
    );
    assert!(
        message.contains(&blocked_path.display().to_string())
            && message.contains("denyModify rule"),
        "the persisted step message must carry the denial attribution: {message}"
    );
    assert!(!blocked_path.exists());
}

#[test]
fn run_cli_backend_emits_provider_pid_between_the_started_and_finished_events() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-pid-audit",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-pid-audit",
        audit.clone(),
        &serde_json::json!({ "prompt": "do it" }),
        None,
    )
    .expect("run succeeds");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
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
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-config",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let mut provider_config = HashMap::new();
    provider_config.insert("sandbox".to_string(), "danger-full-access".to_string());
    provider_config.insert("approval_policy".to_string(), "never".to_string());
    provider_config.insert(
        "writable_dirs_json".to_string(),
        r#"["/tmp/orbit-a","/tmp/orbit-b"]"#.to_string(),
    );
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config,
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-config",
        audit.clone(),
        &serde_json::json!({ "prompt": "do it" }),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    let events = audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => Some(argv_redacted),
            _ => None,
        })
        .expect("cli.invocation.started event");

    assert_eq!(
        argv,
        &vec![
            script.display().to_string(),
            "--config".to_string(),
            "approval_policy=\"never\"".to_string(),
            "--sandbox".to_string(),
            "danger-full-access".to_string(),
            "--add-dir".to_string(),
            "/tmp/orbit-a".to_string(),
            "--add-dir".to_string(),
            "/tmp/orbit-b".to_string(),
        ]
    );
}

#[test]
fn run_cli_backend_passes_model_to_grok_and_captures_well_formed_stdout() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"pong\":\"grok-smoke\"},\"error\":null}",
        "stopReason": "EndTurn"
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{grok_stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-model",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: vec![
            "--output-format".to_string(),
            "json".to_string(),
            "--prompt-file".to_string(),
            "/dev/stdin".to_string(),
        ],
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-model",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

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

    let events = audit.events_snapshot().expect("events snapshot");
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

#[test]
fn run_cli_backend_uses_grok_final_text_not_wrapper_metadata() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"final-text\"},\"error\":null}",
        "stopReason": "EndTurn",
        "thought": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"metadata\",\"message\":\"ignore\",\"details\":null}}",
        "toolCalls": [{"result": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"tool\",\"message\":\"ignore\",\"details\":null}}"}],
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{grok_stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-final-text",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.require_completion_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-final-text",
        audit,
        &serde_json::json!({"prompt": "respond"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["source"], "final-text");
    assert_eq!(outcome.output["response_envelope_status"], "success");
}

#[test]
fn run_cli_backend_preserves_grok_failed_final_text() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"final_failure\",\"message\":\"final answer failed\",\"details\":null}}",
        "stopReason": "EndTurn",
        "thought": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"metadata\"},\"error\":null}",
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{grok_stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-final-failure",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.require_completion_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-final-failure",
        audit,
        &serde_json::json!({"prompt": "respond"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["response_envelope_status"], "failed");
    let message = outcome.message.expect("failed final answer diagnostic");
    assert!(message.contains("final_failure"), "{message}");
}

/// Regression for T20260508-17: a structured-output activity that opts into
/// strict response validation must demote an exit-0 subprocess whose embedded
/// Orbit response reports `status: "failed"`.
#[test]
fn run_cli_backend_demotes_success_when_envelope_reports_failed_despite_exit_zero() {
    let temp = tempdir().expect("tempdir");
    // The agent config layer infers provider from the command basename, so
    // the script name must match a known provider. The demotion logic is
    // provider-agnostic — codex exercises the same code path as claude.
    let script = temp.path().join("codex");
    // Stdout shape mirrors the observed Claude CLI failure: a wrapping JSON
    // whose `result` string starts with prose before embedding an Orbit
    // envelope with status="failed". Exit 0.
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": concat!(
            "I could not continue after the workspace disappeared.\n",
            r#"{"schemaVersion":1,"status":"failed","error":{"code":"workspace_unavailable","message":"worktree missing","details":null}}"#
        ),
        "usage": {
            "input_tokens": 1,
            "output_tokens": 1
        }
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-success-demote",
        "claude:s",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.require_response_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-success-demote",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");

    assert!(
        !outcome.success,
        "envelope status=failed must demote dispatch success even on exit 0"
    );
    let message = outcome.message.expect("expected demote message");
    assert!(
        message.contains("envelope status") && message.contains("failed"),
        "demote message should explain envelope status; got {message:?}"
    );
}

/// Sanity check that the demotion does not regress the happy path: an exit-0
/// run with a `status: "success"` envelope must still be classified as
/// success. Without this, the demotion logic could silently flip every
/// claude run to failed.
#[test]
fn run_cli_backend_keeps_success_when_envelope_reports_success() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{}}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-success-keep",
        "claude:s",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-success-keep",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");
    assert!(
        outcome.success,
        "envelope status=success must keep dispatch success on exit 0"
    );
}

/// Crew-driven regression test for ORB-00080 AC #15: a mixed fixture crew must
/// produce `--model claude-opus-4-7` for planner and `--model gpt-5.5` for
/// implementer (identity attribution stays family; no leakage of family name
/// into the --model flag that reaches the CLI).
#[test]
fn single_crew_drives_exact_model_to_agent() {
    let temp = tempdir().expect("tempdir");
    let codex_script = temp.path().join("codex");
    write_executable(
        &codex_script,
        "#!/bin/sh\nprintf '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}\\n'\n",
    );

    let sink_for_writer_i: Arc<dyn AuditSink> = Arc::new(RecordingSink::default());
    let audit_i = Arc::new(V2AuditWriter::new(
        "job-crew-impl",
        format!("codex:{TEST_CODEX_MODEL}"),
        sink_for_writer_i,
    ));
    let host_i = TestHost::with_command(codex_script.display().to_string());
    let spec_i = test_agent_loop_spec_for("codex", Duration::from_secs(5));
    let input_i = serde_json::json!({
        "prompt": "implement",
        "crew": "single-fixture",
        "task_id": "T-crew"
    });
    let resolved_i = resolve_crew_settings(&host_i, &spec_i, &input_i, &input_i)
        .expect("crew resolution")
        .expect("fixture crew config");
    assert_eq!(resolved_i.model.as_deref(), Some(TEST_CODEX_MODEL));
    let mut spec_i_run = spec_i.clone();
    apply_resolved_settings(&mut spec_i_run, &resolved_i);
    let _ = run_cli_backend(
        &host_i,
        &spec_i_run,
        "test_activity",
        "job-crew-impl",
        audit_i.clone(),
        &input_i,
        None,
    )
    .expect("implementer cli run");

    let events_i = audit_i.events_snapshot().expect("impl events");
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
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}\\n'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-argv-redaction",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let audit_for_assert = Arc::clone(&audit);

    let mut host = TestHost::with_command(script.display().to_string());
    host.executor_args = vec![
        "--api-key".to_string(),
        "sk-secretargvtoken1234567890abcdef".to_string(),
    ];
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-argv-redaction",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");
    assert!(outcome.success);

    let events = audit_for_assert.events_snapshot().expect("audit snapshot");
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
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("agy");
    write_executable(&script, "#!/bin/sh\ncat > /dev/null\nexit 0\n");

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-agy-print-timeout",
        "antigravity:gemini-3.8-flash-high",
        sink_for_writer,
    ));
    let audit_for_assert = Arc::clone(&audit);
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("antigravity", Duration::from_secs(3 * 60 * 60));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-agy-print-timeout",
        audit,
        &serde_json::json!({"prompt": "do it"}),
        None,
    )
    .expect("run cli backend");
    assert!(!outcome.success);

    let events = audit_for_assert.events_snapshot().expect("audit snapshot");
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

#[test]
fn run_cli_backend_surfaces_antigravity_timeout_terminal_error_when_stderr_empty() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("agy");
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "status": "ERROR",
            "response": "secret-transcript should not appear in diagnostics",
            "error": "timeout waiting for response"
        }
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{stdout}'\nexit 1\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-agy-timeout-error",
        "antigravity:gemini-3.8-flash-high",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("antigravity", Duration::from_secs(60));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-agy-timeout-error",
        audit,
        &serde_json::json!({"prompt": "do it"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["timed_out"], false);
    assert_eq!(outcome.output["exit_code"], 1);
    let message = outcome.message.expect("provider diagnostic");
    assert!(
        message.contains("timeout waiting for response"),
        "{message}"
    );
    assert!(
        message.contains("cli subprocess exited with code Some(1)"),
        "{message}"
    );
    assert!(
        !message.contains("secret-transcript"),
        "response transcript leaked into diagnostics: {message}"
    );
}

/// [ORB-10746] The `error_max_turns` ending: exit 0, `is_error: true`, no
/// envelope in either `result` or `structured_output`. Structured output stops
/// a model from *answering in prose*; it cannot stop a run from hitting its
/// turn limit. The step must still fail — and must now say why, instead of
/// leaving an operator to explain a full-cost run from the generic message.
#[test]
fn run_cli_backend_names_the_terminal_reason_on_an_exit_zero_error_ending() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let stdout = serde_json::json!({
        "is_error": true,
        "subtype": "error_max_turns",
        "terminal_reason": "max_turns",
        "num_turns": 200,
        "total_cost_usd": 4.17,
        "result": Option::<String>::None,
        "structured_output": Option::<String>::None
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-max-turns",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-max-turns",
        audit,
        &serde_json::json!({"task_id": "ORB-10746"}),
        None,
    )
    .expect("run cli backend");

    // The decision is unchanged from ORB-10449; only the message improves.
    assert!(!outcome.success, "a turn-limit ending must not checkpoint");
    assert_eq!(outcome.output["exit_code"], 0);
    assert_eq!(outcome.output["completion_envelope_satisfied"], false);
    let message = outcome.message.expect("terminal ending message");
    assert!(message.contains("agent step did not complete"), "{message}");
    assert!(message.contains("error_max_turns"), "{message}");
    assert!(message.contains("max_turns"), "{message}");
}

/// A claude build without `--json-schema` rejects it at argument parsing, so
/// the run fails before any agent work — and before any cost. The whole point
/// of failing this early is lost if the operator only sees an exit code.
#[test]
fn run_cli_backend_reports_a_missing_json_schema_flag_as_a_capability_failure() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\n\
         printf '%s\\n' \"error: unknown option '--json-schema'\" >&2\nexit 1\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-missing-flag",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-missing-flag",
        audit,
        &serde_json::json!({"task_id": "ORB-10746"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    let message = outcome.message.expect("capability message");
    assert!(
        message.contains("does not support --json-schema"),
        "{message}"
    );
    assert!(message.contains("no agent work ran"), "{message}");
}

/// The other half of the capability story: a CLI that accepts the flag but
/// whose API rejects the schema fails mid-run, with the evidence in the
/// response wrapper rather than on stderr. `subtype` still reads `"success"`
/// in this shape, so nothing may key on it.
#[test]
fn run_cli_backend_reports_a_rejected_schema_from_the_response_wrapper() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let stdout = serde_json::json!({
        "is_error": true,
        "subtype": "success",
        "structured_output": Option::<String>::None,
        "result": "API Error: 400 tools.0.custom.input_schema: input_schema does not support \
                   oneOf, allOf, or anyOf at the top level"
    })
    .to_string();
    let stdout_file = temp.path().join("stdout.json");
    fs::write(&stdout_file, &stdout).expect("write rejection fixture");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat '{}'\nexit 1\n",
            stdout_file.display()
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-rejected-schema",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-rejected-schema",
        audit,
        &serde_json::json!({"task_id": "ORB-10746"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    let message = outcome.message.expect("schema rejection message");
    assert!(
        message.contains("rejected Orbit's response-envelope schema"),
        "{message}"
    );
    assert!(message.contains("input_schema"), "{message}");
}

/// [ORB-10746] The prevented shape, end to end: a tool-using run that would
/// once have ended in prose now terminates with the schema-validated envelope
/// in `structured_output`, and the step checkpoints. Verified against Claude
/// Code 2.1.220, whose reply carried `stop_reason: "tool_use"` — the exact
/// condition under which ORB-10734 produced prose.
#[test]
fn run_cli_backend_accepts_a_structured_output_envelope_from_a_tool_using_run() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let stdout = serde_json::json!({
        "is_error": false,
        "stop_reason": "tool_use",
        "num_turns": 20,
        "session_id": "44a7dbc8-333e-4852-aaf5-b61d8f4db174",
        "total_cost_usd": 0.2455644,
        "usage": {
            "input_tokens": 154,
            "cache_creation_input_tokens": 19922,
            "cache_read_input_tokens": 714235,
            "output_tokens": 3372
        },
        "terminal_reason": "completed",
        "subtype": "success",
        // Claude emits the validated envelope in both places; the object is
        // the authoritative one.
        "result": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"summary\":\"done\"},\"error\":null}",
        "structured_output": {
            "schemaVersion": 1,
            "status": "success",
            "result": {"summary": "done"},
            "error": null
        }
    })
    .to_string();
    let stdout_file = temp.path().join("stdout.json");
    fs::write(&stdout_file, &stdout).expect("write structured-output fixture");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat '{}'\n",
            stdout_file.display()
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-structured-output",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-structured-output",
        audit,
        &serde_json::json!({"task_id": "ORB-10734"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
    assert_eq!(outcome.output["response_envelope_status"], "success");
}
