#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use orbit_exec::sandbox_exec_program_for_audit;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::run_cli_backend;
use super::test_support::{
    RecordingSink, TestHost, sandbox_exec_can_apply_for_test, sandbox_for_test,
    test_agent_loop_spec_for, write_executable,
};

#[cfg(target_os = "macos")]
#[test]
fn sandboxed_codex_loads_the_public_ca_bundle_while_private_keychains_stay_denied() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    let keychains = home.join("Library/Keychains");
    std::fs::create_dir_all(&keychains).expect("create synthetic keychain directory");
    std::fs::write(keychains.join("login.keychain-db"), "private fixture")
        .expect("write synthetic private credential");
    let _environment = orbit_common::test_env::scoped([
        ("HOME", Some(home.to_string_lossy().as_ref())),
        ("CODEX_CA_CERTIFICATE", None),
        ("SSL_CERT_FILE", None),
    ]);

    let script = temp.path().join("codex");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$CODEX_CA_CERTIFICATE" != /etc/ssl/cert.pem ]; then
  echo "unexpected CODEX_CA_CERTIFICATE=$CODEX_CA_CERTIFICATE" >&2
  exit 31
fi
if [ "${SSL_CERT_FILE+x}" = x ]; then
  echo "SSL_CERT_FILE should remain unset" >&2
  exit 32
fi
if ! /usr/bin/openssl x509 -in "$CODEX_CA_CERTIFICATE" -noout >/dev/null; then
  echo "could not load a certificate from the public CA bundle" >&2
  exit 33
fi
if /bin/cat "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1; then
  echo "private keychain unexpectedly readable" >&2
  exit 34
fi
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-codex-ca-load",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: Some(sandbox_for_test()),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec_for("codex", Duration::from_secs(5)),
        "test_activity",
        "job-codex-ca-load",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("sandboxed Codex fixture loads public CA material");

    assert!(outcome.success);
}

#[cfg(target_os = "macos")]
#[test]
fn run_cli_backend_audit_argv_starts_with_sandbox_exec_for_each_provider() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    for provider_name in ["claude", "codex", "gemini", "grok"] {
        let temp = tempdir().expect("tempdir");
        let script = temp.path().join(provider_name);
        write_executable(
            &script,
            "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
        );

        let sink = Arc::new(RecordingSink::default());
        let sink_for_writer: Arc<dyn AuditSink> = sink;
        let audit = Arc::new(V2AuditWriter::new(
            "job-sandbox-shape",
            format!("{provider_name}:m"),
            sink_for_writer,
        ));
        let host = TestHost {
            command: script.display().to_string(),
            executor_args: Vec::new(),
            provider_config: HashMap::new(),
            sandbox: Some(sandbox_for_test()),
            task_context: None,
            workspace_root: None,
            orbit_registry_root: None,
            orbit_workspace_selector: None,
        };
        let spec = test_agent_loop_spec_for(provider_name, Duration::from_secs(5));

        let outcome = run_cli_backend(
            &host,
            &spec,
            "test_activity",
            "job-sandbox-shape",
            audit.clone(),
            &serde_json::json!({"prompt": "hi"}),
            None,
        )
        .expect("run cli backend");
        assert!(
            outcome.success,
            "provider {provider_name} cli backend failed"
        );

        let events = audit.events_snapshot().expect("events snapshot");
        let argv = events
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                    Some(argv_redacted.clone())
                }
                _ => None,
            })
            .expect("cli.invocation.started event");
        assert_eq!(
            &argv[..3],
            &[
                sandbox_exec_program_for_audit().to_string(),
                "-f".to_string(),
                "<profile.sb>".to_string()
            ],
            "provider {provider_name} should log trusted sandbox-exec prefix; argv={argv:?}"
        );
        assert_eq!(
            argv[3],
            script.display().to_string(),
            "provider {provider_name} should log program after sandbox prefix"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn run_cli_backend_pins_codex_sandbox_under_outer_wrapper() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-codex-pin",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let mut provider_config = HashMap::new();
    provider_config.insert("sandbox".to_string(), "workspace-write".to_string());
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config,
        sandbox: Some(sandbox_for_test()),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec_for("codex", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-codex-pin",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("cli.invocation.started event");
    let mut idx = None;
    for (i, value) in argv.iter().enumerate() {
        if value == "--sandbox" {
            idx = Some(i);
            break;
        }
    }
    let i = idx.expect("argv must include --sandbox");
    assert_eq!(
        argv.get(i + 1).map(String::as_str),
        Some("danger-full-access"),
        "codex --sandbox must be pinned to danger-full-access; argv={argv:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn run_cli_backend_drops_gemini_sandbox_flag_under_outer_wrapper() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("gemini");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-gemini-drop",
        "gemini:gemini-3.1-pro",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: vec![
            "--approval-mode".to_string(),
            "yolo".to_string(),
            "--sandbox".to_string(),
            "-o".to_string(),
            "json".to_string(),
        ],
        provider_config: HashMap::new(),
        sandbox: Some(sandbox_for_test()),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec_for("gemini", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-gemini-drop",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("cli.invocation.started event");
    // Skip `<trusted sandbox-exec> -f <profile.sb> <program>` prefix so the
    // assertion targets the gemini-side argv.
    let suffix = &argv[4..];
    assert!(
        !suffix.iter().any(|a| a == "--sandbox" || a == "-s"),
        "gemini argv suffix must not contain --sandbox / -s: {suffix:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn run_cli_backend_drops_grok_sandbox_flag_under_outer_wrapper() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-drop",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: vec![
            "--sandbox".to_string(),
            "workspace-write".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ],
        provider_config: HashMap::new(),
        sandbox: Some(sandbox_for_test()),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-drop",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("cli.invocation.started event");
    let suffix = &argv[4..];
    assert!(
        !suffix.iter().any(|a| a == "--sandbox") && !suffix.iter().any(|a| a == "workspace-write"),
        "grok argv suffix must not contain --sandbox profile: {suffix:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn run_cli_backend_leaves_claude_argv_suffix_unchanged_under_sandbox() {
    if !sandbox_exec_can_apply_for_test() {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-claude-passthrough",
        "claude:claude-opus-4-7",
        sink_for_writer,
    ));
    let claude_static_args = vec![
        "-p".to_string(),
        "--permission-mode".to_string(),
        "bypassPermissions".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ];
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: claude_static_args.clone(),
        provider_config: HashMap::new(),
        sandbox: Some(sandbox_for_test()),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-claude-passthrough",
        audit.clone(),
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");
    assert!(outcome.success);

    let events = audit.events_snapshot().expect("events snapshot");
    let argv = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { argv_redacted, .. } => {
                Some(argv_redacted.clone())
            }
            _ => None,
        })
        .expect("cli.invocation.started event");
    let suffix = &argv[4..4 + claude_static_args.len()];
    assert_eq!(
        suffix,
        claude_static_args.as_slice(),
        "claude static args must pass through unchanged"
    );
}
