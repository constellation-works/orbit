#![allow(missing_docs)]

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::activity_job::{load_activity_asset, load_job_asset};
use orbit_agent::loop_engine::audit::AuditSink;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_store::Store;
use orbit_types::workflow::activity_job::{ActivityV2Spec, JobV2StepBody, V2AuditEventKind};
use tempfile::{TempDir, tempdir};

use crate::context::{ProvenanceEnv, provenance_env};
use crate::template::{self, TemplateContext};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::super::crew::{apply_resolved_settings, resolve_crew_settings};
use super::super::super::dispatcher::DispatchError;
#[cfg(target_os = "linux")]
use super::super::super::dispatcher::ResolvedSandbox;
use super::super::super::sqlite_sink::V2SqliteSink;
use super::super::super::workspace::{WorktreeBoundaryGuard, validate_declared_worktree_pair};
use super::super::orchestrator::{provider_child_environment, resolved_activity_fs_profile_name};
use super::super::run_cli_backend;
use super::test_support::{
    RecordingSink, TestHost, capture_events, sandbox_for_test, test_agent_loop_spec,
    test_agent_loop_spec_for, write_executable,
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
    let host = TestHost::with_command(script.display().to_string());
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
fn run_cli_backend_does_not_project_codex_command_output_as_response() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}'\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-0\",\"type\":\"command_execution\",\"command\":\"read fixture\",\"aggregated_output\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"claimed\\\":\\\"tool-output\\\"},\\\"error\\\":null}\",\"exit_code\":0,\"status\":\"completed\"}}'\n",
            "printf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":17,\"output_tokens\":3}}'\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-codex-command-only",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-codex-command-only",
        audit,
        &serde_json::json!({"prompt": "read the fixture"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert!(outcome.output["response_envelope_status"].is_null());
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert!(outcome.output.get("claimed").is_none());
    assert!(
        outcome.output["stdout_text"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("tool-output")),
        "raw stdout remains available for diagnostics"
    );
}

#[test]
fn run_cli_backend_projects_codex_final_answer_and_keeps_raw_trace() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-0\",\"type\":\"command_execution\",\"command\":\"read fixture\",\"aggregated_output\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"failed\\\",\\\"result\\\":{},\\\"error\\\":{\\\"code\\\":\\\"fixture\\\",\\\"message\\\":\\\"tool-output\\\"}}\",\"exit_code\":0,\"status\":\"completed\"}}'\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-1\",\"type\":\"agent_message\",\"text\":\"Commentary: I inspected the task.\"}}'\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-8\",\"type\":\"agent_message\",\"text\":\"Commentary: I updated the files.\"}}'\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-12\",\"type\":\"agent_message\",\"text\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"source\\\":\\\"assistant\\\"},\\\"error\\\":null}\"}}'\n",
            "printf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":101,\"cached_input_tokens\":11,\"output_tokens\":9}}'\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-codex-final-answer",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-codex-final-answer",
        audit,
        &serde_json::json!({"prompt": "read then answer"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["source"], "assistant");
    assert_eq!(outcome.output["response_envelope_status"], "success");
    let trace = &outcome
        .invocation
        .as_ref()
        .expect("raw invocation trace")
        .trace;
    assert_eq!(trace.usage.input, 101);
    assert_eq!(trace.usage.cache_read, 11);
    assert_eq!(trace.usage.output, 9);
    assert_eq!(trace.tool_calls.len(), 1);
    assert!(
        outcome.output["stdout_text"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("tool-output")),
        "raw stdout remains available for diagnostics"
    );
}

#[test]
fn run_cli_backend_rejects_an_invalid_terminal_codex_answer() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-1\",\"type\":\"agent_message\",\"text\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"source\\\":\\\"earlier\\\"},\\\"error\\\":null}\"}}'\n",
            "printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"item-2\",\"type\":\"agent_message\",\"text\":\"not valid JSON\"}}'\n",
            "printf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":21,\"output_tokens\":8}}'\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-codex-invalid-terminal-answer",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-codex-invalid-terminal-answer",
        audit,
        &serde_json::json!({"prompt": "read then answer"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert!(outcome.output.get("source").is_none());
}

#[test]
fn run_cli_backend_copilot_cancellation_cannot_project_tool_arguments() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"assistant.reasoning\",\"data\":{\"content\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"claimed\\\":\\\"reasoning\\\"},\\\"error\\\":null}\"}}'\n",
            "printf '%s\\n' '{\"type\":\"assistant.message\",\"data\":{\"content\":\"\",\"toolRequests\":[{\"toolCallId\":\"call-1\",\"name\":\"shell\",\"arguments\":{\"command\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"claimed\\\":\\\"tool-arguments\\\"},\\\"error\\\":null}\"}}]}}'\n",
            "printf '%s\\n' '{\"type\":\"session.abort\",\"data\":{\"reason\":\"cancelled\"},\"ephemeral\":true}'\n",
            "exit 130\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-copilot-cancelled",
        "copilot:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-copilot-cancelled",
        audit,
        &serde_json::json!({"prompt": "cancel after tool request"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert!(outcome.output["response_envelope_status"].is_null());
    assert!(outcome.output.get("claimed").is_none());
    assert!(
        outcome.output["stdout_text"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("tool-arguments")),
        "raw stdout remains available for diagnostics"
    );
}

#[test]
fn run_cli_backend_projects_copilot_final_answer_and_keeps_usage() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"assistant.message\",\"data\":{\"content\":\"\",\"toolRequests\":[{\"toolCallId\":\"call-1\",\"name\":\"shell\",\"arguments\":{\"command\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"failed\\\"}\"}}]}}'\n",
            "printf '%s\\n' '{\"type\":\"assistant.message\",\"data\":{\"content\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{\\\"source\\\":\\\"assistant\\\"},\\\"error\\\":null}\"}}'\n",
            "printf '%s\\n' '{\"type\":\"assistant.usage\",\"data\":{\"inputTokens\":73,\"outputTokens\":12}}'\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-copilot-final-answer",
        "copilot:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-copilot-final-answer",
        audit,
        &serde_json::json!({"prompt": "use a tool then answer"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["source"], "assistant");
    assert_eq!(outcome.output["response_envelope_status"], "success");
    let usage = &outcome
        .invocation
        .as_ref()
        .expect("normalized invocation trace")
        .trace
        .usage;
    assert_eq!(usage.input, 73);
    assert_eq!(usage.output, 12);
}

#[test]
fn run_cli_backend_rejects_copilot_terminal_failed_or_timeout_after_commentary() {
    let temp = tempdir().expect("tempdir");

    for status in ["failed", "timeout"] {
        let script = temp.path().join("copilot");
        let commentary = serde_json::json!({
            "type": "assistant.message",
            "data": {"content": "Commentary: I updated the files."},
        });
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "status": status,
            "result": {},
            "error": {"code": "fixture", "message": status},
        });
        let terminal = serde_json::json!({
            "type": "assistant.message",
            "data": {"content": envelope.to_string()},
        });
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{commentary}'\nprintf '%s\\n' '{terminal}'\n"
            ),
        );

        let sink = Arc::new(RecordingSink::default());
        let sink_for_writer: Arc<dyn AuditSink> = sink;
        let audit = Arc::new(V2AuditWriter::new(
            format!("job-copilot-{status}-after-commentary"),
            "copilot:gpt-5.5",
            sink_for_writer,
        ));
        let host = TestHost::with_command(script.display().to_string());
        let mut spec = test_agent_loop_spec(Duration::from_secs(5));
        spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

        let outcome = run_cli_backend(
            &host,
            &spec,
            "test_activity",
            &format!("job-copilot-{status}-after-commentary"),
            audit,
            &serde_json::json!({"prompt": "answer after progress"}),
            None,
        )
        .expect("run cli backend");

        assert!(
            !outcome.success,
            "{status} terminal envelope must fail the step"
        );
        assert_eq!(outcome.output["response_envelope_status"], status);
        assert_eq!(outcome.output["completion_envelope_satisfied"], true);
    }
}

#[test]
fn run_cli_backend_rejects_copilot_trailing_terminal_prose() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' '{\"type\":\"assistant.message\",\"data\":{\"content\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{},\\\"error\\\":null}\"}}'\n",
            "printf '%s\\n' '{\"type\":\"assistant.message\",\"data\":{\"content\":\"Courtesy: the run is complete.\"}}'\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-copilot-trailing-prose",
        "copilot:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-copilot-trailing-prose",
        audit,
        &serde_json::json!({"prompt": "answer then add courtesy prose"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert!(outcome.output["response_envelope_status"].is_null());
    assert_eq!(outcome.output["completion_envelope_satisfied"], false);
}

/// A Copilot CLI that cannot see its login-keychain item exits 1 with
/// "No authentication information found". The step message — which becomes
/// `job_run_steps.error_message` and the task's `workflow_run_failed` note —
/// must carry Orbit's diagnosis, not only the exit code. [ORB-12261]
#[test]
fn run_cli_backend_copilot_keychain_auth_failure_reaches_the_step_message() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' 'Error: No authentication information found.' >&2\n",
            "exit 1\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-copilot-keychain",
        "copilot:claude-sonnet-4.5",
        sink_for_writer,
    ));
    let mut sandbox = sandbox_for_test();
    // Linux CI has no sandbox-exec; the diagnostic is about the compiled
    // profile, not about whether the wrapper actually applied.
    sandbox.allow_fallback = true;
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: Some(sandbox),
        task_context: None,
        workspace_root: None,
        orbit_registry_root: None,
        orbit_workspace_selector: None,
    };
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "implement_one",
        "jrun-copilot-keychain",
        audit,
        &serde_json::json!({"prompt": "say ok"}),
        None,
    )
    .expect("the invocation outcome should be classified");

    assert!(!outcome.success);
    let message = outcome
        .message
        .as_deref()
        .expect("failed step carries a message");
    assert!(
        message.contains("exited with code"),
        "the exit code still belongs in the step message: {message}"
    );
    assert!(
        message.contains("macOS sandbox") && message.contains("copilot"),
        "the persisted step message must carry the keychain diagnosis, not only the exit code: {message}"
    );

    let update = crate::context::blocked_workflow_failure_update(
        "task_pr_pipeline",
        "jrun-copilot-keychain",
        Some("AGENT_INVOCATION_FAILED"),
        Some(message),
    );
    assert_eq!(
        update.status_event.as_deref(),
        Some(crate::context::WORKFLOW_RUN_FAILED_EVENT)
    );
    let note = update
        .status_note
        .as_deref()
        .expect("blocked update carries a note");
    assert!(
        note.contains("macOS sandbox") && note.contains("copilot"),
        "workflow_run_failed must inline the diagnosis: {note}"
    );
}

#[test]
fn run_cli_backend_projects_prose_prefixed_claude_envelope_result() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let response = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": format!(
            "I classified both failed runs.\n{}",
            serde_json::json!({
                "schemaVersion": 1,
                "status": "success",
                "result": {
                    "dispositions": [
                        {
                            "task_id": "ORB-A",
                            "classification": "environmental",
                            "disposition": "rebacklog",
                            "diagnosis": "stale worktree removed"
                        },
                        {
                            "task_id": "ORB-B",
                            "classification": "code_defect",
                            "disposition": "stay_blocked",
                            "diagnosis": "tests remain red"
                        }
                    ],
                    "summary": "one recovery and one human follow-up"
                },
                "error": null
            })
        ),
        "usage": {
            "input_tokens": 11,
            "output_tokens": 7
        }
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{response}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-claude-envelope-result",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-claude-envelope-result",
        audit,
        &serde_json::json!({"prompt": "triage failed runs"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success);
    assert_eq!(
        outcome.output["dispositions"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        outcome.output["dispositions"][0]["disposition"],
        serde_json::json!("rebacklog")
    );
    assert_eq!(
        outcome.output["dispositions"][1]["disposition"],
        serde_json::json!("stay_blocked")
    );
    assert_eq!(
        outcome.output["summary"],
        serde_json::json!("one recovery and one human follow-up")
    );
    assert_eq!(outcome.output["provider"], serde_json::json!("claude"));
}

#[test]
fn run_cli_backend_rejects_schema_invalid_success_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":2,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-invalid-envelope",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.require_response_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-invalid-envelope",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    let message = outcome.message.expect("invalid envelope message");
    assert!(
        message.contains("cli response envelope invalid"),
        "{message}"
    );
    assert!(
        message.contains("unsupported schemaVersion: 2"),
        "{message}"
    );
}

/// [ORB-10449] The regression this task exists for: an agent that exits 0 with
/// prose and no envelope did not finish its turn, and must not checkpoint as
/// success just because the activity never opted into the *content* contract.
#[test]
fn run_cli_backend_fails_artifact_activity_when_exit_zero_carries_no_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "All changes are complete and validated. Execution summary persisted to the task.",
        "stop_reason": "end_turn"
    })
    .to_string();
    write_executable(
        &script,
        &format!("#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{stdout}'\n"),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-artifact-response",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-artifact-response",
        audit,
        &serde_json::json!({"task_id": "ORB-10230"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["exit_code"], 0);
    // The content contract is still opt-in and still off here — the step failed
    // on the completion protocol alone.
    assert_eq!(outcome.output["response_envelope_required"], false);
    assert_eq!(outcome.output["completion_envelope_required"], true);
    assert_eq!(outcome.output["completion_envelope_satisfied"], false);
    let message = outcome.message.expect("completion protocol message");
    assert!(message.contains("agent step did not complete"), "{message}");
    assert!(
        message.contains("does not contain an Orbit response envelope"),
        "{message}"
    );
}

/// The declared exception (`dispatch_agent`): an activity whose work is
/// decorative keeps the pre-ORB-10449 behaviour, and the invalid envelope is
/// still recorded as a diagnostic rather than acted on.
#[test]
fn run_cli_backend_keeps_advisory_activity_successful_without_an_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' 'advisory grouping notes, no envelope'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-advisory-response",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));
    spec.require_completion_envelope = false;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-advisory-response",
        audit,
        &serde_json::json!({"prompt": "group the backlog"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success);
    assert!(outcome.message.is_none());
    assert_eq!(outcome.output["completion_envelope_required"], false);
    assert_eq!(outcome.output["completion_envelope_satisfied"], false);
    assert!(
        outcome.output["completion_envelope_error"]
            .as_str()
            .is_some_and(|message| message.contains("agent step did not complete"))
    );
}

/// The decorative opt-out suppresses both completion gates. A failed token is
/// recorded for diagnostics but remains advisory when no contract consumes it.
#[test]
fn run_cli_backend_keeps_opted_out_declared_failure_advisory() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"decorative\",\"message\":\"ignored\"}}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-advisory-declared-failure",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));
    spec.require_completion_envelope = false;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-advisory-declared-failure",
        audit,
        &serde_json::json!({"prompt": "emit decorative status"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success);
    assert!(outcome.message.is_none());
    assert_eq!(outcome.output["response_envelope_status"], "failed");
    assert_eq!(outcome.output["completion_envelope_required"], false);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
}

/// The completion frame remains content-blind, but an explicit failed status
/// is the invocation's control-plane outcome. It must fail a required
/// completion contract without making its `result` or `error` advisory prose
/// authoritative.
#[test]
fn run_cli_backend_completion_gate_demotes_a_declared_failure_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    let envelope = serde_json::json!({
        "schemaVersion": 1,
        "status": "failed",
        "result": {},
        "error": {
            "code": "macos_validation_unavailable",
            "message": format!(
                "sandbox-exec is unavailable; token=sk-test-redaction {}",
                "x".repeat(2 * 1024)
            ),
        },
    });
    let stdout_file = temp.path().join("stdout.json");
    fs::write(&stdout_file, envelope.to_string()).expect("write envelope fixture");
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
        "job-declared-failure",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-declared-failure",
        audit,
        &serde_json::json!({"task_id": "ORB-10449"}),
        None,
    )
    .expect("run cli backend");

    assert!(
        !outcome.success,
        "a declared failed status must not checkpoint a required completion"
    );
    assert_eq!(outcome.output["completion_envelope_required"], true);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
    assert!(outcome.output["completion_envelope_error"].is_null());
    assert_eq!(outcome.output["response_envelope_status"], "failed");
    let message = outcome.message.expect("declared failure message");
    assert!(message.contains("declared envelope status"), "{message}");
    assert!(message.contains("failed"), "{message}");
    assert!(
        message.contains("error.code=macos_validation_unavailable"),
        "{message}"
    );
    assert!(message.contains("sandbox-exec is unavailable"), "{message}");
    assert!(!message.contains("sk-test-redaction"), "{message}");
    assert!(
        message.len() < 1_300,
        "diagnostic must remain bounded: {message}"
    );
}

/// `timeout` is just as terminal as `failed` when the provider reports it in
/// the completed Orbit envelope, even though the process itself exited 0.
#[test]
fn run_cli_backend_completion_gate_demotes_a_declared_timeout_envelope() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"timeout\",\"result\":{},\"error\":{\"code\":\"deadline\",\"message\":\"timed out\"}}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-declared-timeout",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-declared-timeout",
        audit,
        &serde_json::json!({"task_id": "ORB-10733"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
    assert_eq!(outcome.output["response_envelope_status"], "timeout");
    let message = outcome.message.expect("declared timeout message");
    assert!(message.contains("declared envelope status"), "{message}");
    assert!(message.contains("timeout"), "{message}");
    assert!(message.contains("error.code=deadline"), "{message}");
    assert!(message.contains("error.message=timed out"), "{message}");
}

#[test]
fn run_cli_backend_demotes_declared_failure_with_missing_or_malformed_error_details() {
    for (fixture, error) in [
        ("missing", serde_json::Value::Null),
        ("malformed", serde_json::json!({"code": 42, "message": []})),
    ] {
        let temp = tempdir().expect("tempdir");
        let script = temp.path().join("codex");
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "status": "failed",
            "result": {},
            "error": error,
        });
        let stdout_file = temp.path().join("stdout.json");
        fs::write(&stdout_file, envelope.to_string()).expect("write envelope fixture");
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
            format!("job-declared-failure-{fixture}"),
            "codex:gpt-5.5",
            sink_for_writer,
        ));
        let host = TestHost::with_command(script.display().to_string());

        let outcome = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(5)),
            "test_activity",
            &format!("job-declared-failure-{fixture}"),
            audit,
            &serde_json::json!({"task_id": "ORB-11439"}),
            None,
        )
        .expect("run cli backend");

        assert!(!outcome.success, "{fixture} error must still demote exit 0");
        assert_eq!(outcome.output["response_envelope_status"], "failed");
        let message = outcome.message.expect("declared failure message");
        assert!(
            message.contains("declared envelope error details unavailable"),
            "{message}"
        );
    }
}

/// A provider that interleaves a wrapped tool's stdout with its own protocol
/// output still terminated properly. The completion gate must key on the
/// termination signal, not on the tidiness of the stream around it — a false
/// positive here would fail completed work.
#[test]
fn run_cli_backend_completion_check_tolerates_interleaved_non_json_stdout() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '[main abc1234] some commit'\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-interleaved-stdout",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-interleaved-stdout",
        audit,
        &serde_json::json!({"task_id": "ORB-10449"}),
        None,
    )
    .expect("run cli backend");

    assert!(outcome.success);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
}

/// [ORB-10449] `jrun-20260726-1758-5` replayed as a fixture: claude exits 0
/// with `stop_reason: end_turn` after parking itself on a background process,
/// having emitted no envelope. Before this change the step checkpointed as
/// success and the run failed three steps later at the delivery gate.
#[test]
fn run_cli_backend_fails_on_the_jrun_20260726_1758_5_stall_shape() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "stop_reason": "end_turn",
        "result": "The nextest run is still executing in the background (no failures through \
                   1782/2693 tests so far). I'll wait for the scheduled wakeup or task \
                   notification before analyzing results and continuing the ORB-10436 audit."
    })
    .to_string();
    // The captured prose contains an apostrophe, so route it through a file
    // rather than a single-quoted shell literal.
    let stdout_file = temp.path().join("stdout.json");
    fs::write(&stdout_file, &stdout).expect("write stall stdout fixture");
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
        "jrun-20260726-1758-5",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    // `agent_implement`'s shipped shape: artifact-backed, so the content
    // contract is off. Only the completion protocol stands between a stalled
    // implementer and a green checkpoint.
    let spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));
    assert!(!spec.require_response_envelope);

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "jrun-20260726-1758-5",
        audit,
        &serde_json::json!({"task_id": "ORB-10436"}),
        None,
    )
    .expect("run cli backend");

    assert!(
        !outcome.success,
        "a stalled implementer must not checkpoint"
    );
    assert_eq!(outcome.output["exit_code"], 0);
    assert_eq!(outcome.output["timed_out"], false);
    let message = outcome.message.expect("stall message");
    assert!(message.contains("agent step did not complete"), "{message}");
}

#[test]
fn run_cli_backend_requires_envelope_when_activity_opts_in() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"completed\"}'\n",
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-required-response",
        "claude:sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec_for("claude", Duration::from_secs(5));
    spec.require_response_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-required-response",
        audit,
        &serde_json::json!({"prompt": "return structured data"}),
        None,
    )
    .expect("run cli backend");

    assert!(!outcome.success);
    assert_eq!(outcome.output["response_envelope_required"], true);
    assert_eq!(outcome.output["response_envelope_valid"], false);
    assert!(
        outcome
            .message
            .as_deref()
            .is_some_and(|message| message.contains("does not contain an Orbit response envelope"))
    );
}

#[test]
fn run_cli_backend_keeps_verbose_provider_result_usable_after_capture_truncation() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
printf '%s' '{"type":"item.completed","item":{"type":"reasoning","text":"'
i=0
while [ "$i" -lt 18000 ]; do
  printf '%s' 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'
  i=$((i + 1))
done
printf '%s\n' '"}}'
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"workflow\":\"usable\"},\"error\":null}"}}'
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "job-verbose-output",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let spec = test_agent_loop_spec(Duration::from_secs(10));

    let (outcome, _events) = capture_events(|| {
        run_cli_backend(
            &host,
            &spec,
            "test_activity",
            "job-verbose-output",
            audit,
            &serde_json::json!({"prompt": "perform verbose work"}),
            None,
        )
    });
    let outcome = outcome.expect("verbose run succeeds");

    assert!(outcome.success, "capture truncation must not fail the run");
    assert!(outcome.message.is_none());
    assert_eq!(outcome.output["exit_code"], 0);
    assert_eq!(outcome.output["timed_out"], false);
    assert_eq!(outcome.output["stdout_capture_truncated"], true);
    let observed = outcome.output["stdout_text_original_bytes"]
        .as_u64()
        .expect("observed byte count");
    let limit = outcome.output["stdout_capture_limit_bytes"]
        .as_u64()
        .expect("capture limit");
    let captured = outcome.output["stdout_text_captured_bytes"]
        .as_u64()
        .expect("captured byte count");
    assert!(observed > limit);
    assert!(captured < observed);

    let preview = outcome.output["stdout_text"]
        .as_str()
        .expect("stdout protocol tail");
    let documents = serde_json::Deserializer::from_str(preview)
        .into_iter::<serde_json::Value>()
        .collect::<Result<Vec<_>, _>>()
        .expect("retained stdout text remains valid JSONL");
    assert_eq!(
        documents
            .last()
            .and_then(|value| value.pointer("/item/text"))
            .and_then(serde_json::Value::as_str),
        Some(
            r#"{"schemaVersion":1,"status":"success","result":{"workflow":"usable"},"error":null}"#
        )
    );

    let stdout_blob_ref = outcome.output["stdout_blob_ref"]
        .as_str()
        .expect("stdout blob ref");
    let stored = sink.blob(stdout_blob_ref).expect("stored bounded stdout");
    assert!(stored.len() < observed as usize);
    assert!(
        String::from_utf8_lossy(&stored).contains("observed_bytes="),
        "bounded blob should record why and where capture was truncated"
    );
}

#[test]
fn run_cli_backend_bounds_stdout_text_preview_and_keeps_envelope_status_from_full_stdout() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("codex");
    let embedded_envelope = r#"{"schemaVersion":1,"status":"failed","error":{"code":"workspace_unavailable","message":"worktree missing","details":null}}"#;
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": format!("{}{}", "x".repeat(70 * 1024), embedded_envelope),
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
        "job-stdout-preview",
        "codex:gpt-5.5",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.require_response_envelope = true;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-stdout-preview",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        !outcome.success,
        "status=failed after the preview limit must still demote success"
    );
    let preview = outcome.output["stdout_text"]
        .as_str()
        .expect("stdout_text preview");
    assert!(preview.len() <= 64 * 1024);
    assert!(!preview.contains("workspace_unavailable"));
    assert_eq!(outcome.output["stdout_text_truncated"], true);
    assert_eq!(
        outcome.output["stdout_text_preview_bytes"].as_u64(),
        Some(preview.len() as u64)
    );
    assert_eq!(
        outcome.output["stdout_text_preview_limit_bytes"].as_u64(),
        Some((64 * 1024) as u64)
    );
    let message = outcome.message.expect("expected demote message");
    assert!(
        message.contains("envelope status") && message.contains("failed"),
        "demote message should explain envelope status; got {message:?}"
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
    let _guard = EnvVarGuard::set("ORBIT_CLI_BLOB_TEST_TOKEN", secret);
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

/// [ORB-10879] The composition rule behind the exit-0 attribution, asserted
/// without Bubblewrap: the spawn-boundary tests below are `#[ignore]`d because
/// a host whose kernel forbids unprivileged user namespaces (including every
/// nested Orbit sandbox) cannot run them, so the message contract gets a check
/// that always runs.
#[test]
fn sandbox_write_attribution_rides_along_with_the_frame_classification() {
    let denial = "Orbit linux-bwrap policy denied the attempted write: \
                  `/w/.orbit/x` is not writable inside the sandbox: denyModify rule `!/w/.orbit/**` shadows it";

    let composed = super::super::orchestrator::with_sandbox_write_attribution(
        "agent step did not complete".to_string(),
        Some(denial),
    );
    assert!(
        composed.starts_with("agent step did not complete"),
        "the frame classification stays the head of the message: {composed}"
    );
    assert!(
        composed.ends_with(denial),
        "the denial text is appended verbatim, not reformatted: {composed}"
    );

    assert_eq!(
        super::super::orchestrator::with_sandbox_write_attribution(
            "agent step did not complete".to_string(),
            None,
        ),
        "agent step did not complete",
        "a step that hit no write denial keeps its message unchanged"
    );
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

const TASK_LOCAL_PIPELINE_YAML: &str =
    include_str!("../../../../../../.orbit/resources/jobs/task_local_pipeline.yaml");
const TASK_PR_PIPELINE_YAML: &str =
    include_str!("../../../../../../.orbit/resources/jobs/task_pr_pipeline.yaml");
const EPIC_PIPELINE_YAML: &str =
    include_str!("../../../../../../.orbit/resources/jobs/epic_pipeline.yaml");
const EPIC_ORCHESTRATOR_YAML: &str =
    include_str!("../../../../../../.orbit/resources/activities/epic_orchestrator.yaml");

#[test]
fn actual_ship_pipeline_implementers_run_for_each_provider_and_stay_in_the_worktree() {
    let mut rendered_assets = BTreeSet::new();
    for pipeline_yaml in [TASK_LOCAL_PIPELINE_YAML, TASK_PR_PIPELINE_YAML] {
        for provider in ["claude", "codex"] {
            let fixture = linked_worktree_fixture();
            let script = fixture.root().join(provider);
            let assigned = fixture.assigned.display().to_string();
            let task_id = format!("ORB-ASSET-{provider}");
            let (pipeline, input) =
                rendered_implement_input_from_asset(pipeline_yaml, &fixture.assigned, &task_id);
            rendered_assets.insert(pipeline.clone());
            write_executable(
                &script,
                &format!(
                    r#"#!/bin/sh
cat > /dev/null
test "$(pwd -P)" = '{assigned}' || exit 41
test "$(git rev-parse --show-toplevel)" = '{assigned}' || exit 42
printf '%s\n' '{pipeline}:{provider}' > observed-relative-write.txt
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{}},"error":null}}'
"#
                ),
            );
            let mut host = TestHost::with_command(script.display().to_string());
            host.workspace_root = Some(fixture.primary.clone());
            let spec = test_agent_loop_spec_for(provider, Duration::from_secs(5));
            let audit = test_audit(&format!("run-{pipeline}-{provider}"), provider);

            let outcome = run_cli_backend(
                &host,
                &spec,
                "test_activity",
                &format!("run-{pipeline}-{provider}"),
                audit,
                &input,
                None,
            )
            .unwrap_or_else(|error| panic!("{pipeline}/{provider} invocation: {error}"));

            assert!(outcome.success, "{pipeline}/{provider} should succeed");
            assert_eq!(
                fs::read_to_string(fixture.assigned.join("observed-relative-write.txt"))
                    .expect("assigned relative write"),
                format!("{pipeline}:{provider}\n")
            );
            assert!(
                !fixture.primary.join("observed-relative-write.txt").exists(),
                "{pipeline}/{provider} must not write the registered primary checkout"
            );
        }
    }
    assert_eq!(
        rendered_assets,
        BTreeSet::from([
            "task_local_pipeline".to_string(),
            "task_pr_pipeline".to_string(),
        ]),
        "the matrix must load and render both committed shipment assets"
    );
}

#[test]
fn epic_orchestrator_finisher_writes_stay_in_the_epic_worktree() {
    let fixture = linked_worktree_fixture();
    let assigned = fixture.assigned.display().to_string();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
test "$(pwd -P)" = '{assigned}' || exit 41
test "$(git rev-parse --show-toplevel)" = '{assigned}' || exit 42
printf '%s\n' 'finisher work' > finisher.txt
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{}},"error":null}}'
"#
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let input = rendered_finish_input_from_epic_pipeline(&fixture.assigned, "ORB-EPIC-FINISH");
    let spec = epic_orchestrator_spec(Duration::from_secs(5));

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "run-epic-finisher",
        test_audit("run-epic-finisher", "codex"),
        &input,
        None,
    )
    .expect("epic finisher invocation");

    assert!(outcome.success);
    assert_eq!(
        fs::read_to_string(fixture.assigned.join("finisher.txt")).expect("assigned finisher write"),
        "finisher work\n"
    );
    assert!(
        !fixture.primary.join("finisher.txt").exists(),
        "finisher must not write the registered primary checkout"
    );
}

#[test]
fn epic_orchestrator_declared_root_mismatch_fails_before_provider_spawn() {
    let fixture = linked_worktree_fixture();
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let mut input =
        rendered_finish_input_from_epic_pipeline(&fixture.assigned, "ORB-EPIC-MISMATCH");
    input["repo_root"] = serde_json::json!(fixture.primary);
    let spec = epic_orchestrator_spec(Duration::from_secs(5));
    let audit = test_audit("run-epic-finisher-mismatch", "codex");

    let error = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "run-epic-finisher-mismatch",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("mismatched epic finisher roots must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-EPIC-MISMATCH",
        "run-epic-finisher-mismatch",
        "different Git checkouts",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_repo_root_mismatch_fails_typed_before_provider_spawn() {
    let fixture = linked_worktree_fixture();
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-repo-root-mismatch", "codex");
    let mut input = worktree_input(&fixture, "ORB-PAIR-MISMATCH");
    input["repo_root"] = serde_json::json!(fixture.primary);

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-repo-root-mismatch",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("mismatched declared pair must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-PAIR-MISMATCH",
        "run-repo-root-mismatch",
        "different Git checkouts",
    );
    assert_pre_spawn_failure(&audit, &marker);

    input["repo_root"] = serde_json::Value::Null;
    let null_error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-null-repo-root",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("a declared null repo_root must not erase the worktree pair");
    assert_worktree_mismatch(
        &null_error,
        "ORB-PAIR-MISMATCH",
        "run-null-repo-root",
        "repo_root must be a non-empty string",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_non_git_checkout_fails_typed_before_provider_spawn() {
    let fixture = linked_worktree_fixture();
    let non_git = fixture.root().join("not-a-repository");
    fs::create_dir(&non_git).expect("create non-Git directory");
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-non-git-pair", "codex");
    let input = serde_json::json!({
        "task_id": "ORB-NON-GIT",
        "workspace_path": non_git,
        "repo_root": non_git,
    });

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-non-git-pair",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("non-Git declared pair must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-NON-GIT",
        "run-non-git-pair",
        "not a Git checkout",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_checkout_from_different_repository_fails_before_provider_spawn() {
    let assigned_fixture = linked_worktree_fixture();
    let registered_fixture = linked_worktree_fixture();
    let marker = assigned_fixture.root().join("provider-started");
    let script = assigned_fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(registered_fixture.primary.clone());
    let audit = test_audit("run-different-common-dir", "codex");

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-different-common-dir",
        audit.clone(),
        &worktree_input(&assigned_fixture, "ORB-DIFFERENT-REPO"),
        None,
    )
    .expect_err("different repositories must fail closed");

    assert_worktree_mismatch(
        &error,
        "ORB-DIFFERENT-REPO",
        "run-different-common-dir",
        "different Git common dirs",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn declared_checkout_cannot_collapse_to_registered_primary() {
    let fixture = linked_worktree_fixture();
    let marker = fixture.root().join("provider-started");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf started > '{}'\nexit 0\n",
            marker.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-primary-collapse", "codex");
    let input = serde_json::json!({
        "task_id": "ORB-PRIMARY-COLLAPSE",
        "workspace_path": fixture.primary,
        "repo_root": fixture.primary,
    });

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-primary-collapse",
        audit.clone(),
        &input,
        None,
    )
    .expect_err("assigned checkout must not collapse to primary");

    assert_worktree_mismatch(
        &error,
        "ORB-PRIMARY-COLLAPSE",
        "run-primary-collapse",
        "collapses to the registered primary",
    );
    assert_pre_spawn_failure(&audit, &marker);
}

#[test]
fn unchanged_pre_dirty_primary_does_not_block_valid_worktree_implementation() {
    let fixture = linked_worktree_fixture();
    fs::write(
        fixture.primary.join("README.md"),
        "pre-existing primary dirtiness\n",
    )
    .expect("dirty primary");
    let primary_before = git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf 'assigned only\\n' > assigned.txt\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-pre-dirty-primary",
        test_audit("run-pre-dirty-primary", "codex"),
        &worktree_input(&fixture, "ORB-PRE-DIRTY"),
        None,
    )
    .expect("unchanged dirty primary is allowed");

    assert!(outcome.success);
    assert!(fixture.assigned.join("assigned.txt").exists());
    assert_eq!(
        git_bytes(&fixture.primary, &["diff", "--binary", "HEAD", "--"]),
        primary_before,
        "the guard must leave pre-existing primary dirtiness byte-for-byte unchanged"
    );
}

#[test]
fn concurrent_primary_fast_forward_does_not_block_disjoint_worktree_changes() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned\\n' > assigned.txt\nprintf 'concurrent\\n' > '{}/concurrent.txt'\ngit -C '{}' add -- concurrent.txt\ngit -C '{}' commit -m concurrent-base\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
            fixture.primary.display(),
            fixture.primary.display(),
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-concurrent-fast-forward",
        test_audit("run-concurrent-fast-forward", "codex"),
        &worktree_input(&fixture, "ORB-CONCURRENT-FF"),
        None,
    )
    .expect("a disjoint same-branch primary fast-forward is benign");

    assert!(outcome.success);
    assert!(fixture.assigned.join("assigned.txt").exists());
    assert!(fixture.primary.join("concurrent.txt").exists());
}

#[test]
fn two_in_flight_worktrees_survive_one_primary_merge_advance() {
    let fixture = linked_worktree_fixture();
    let assigned_two = fixture.root().join("assigned-two");
    git_ok(
        &fixture.primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-integrity-test-two",
            assigned_two.to_str().expect("utf8 second worktree"),
        ],
    );
    let assigned_two = assigned_two
        .canonicalize()
        .expect("canonical second worktree");
    let input_one = worktree_input(&fixture, "ORB-CONCURRENT-ONE");
    let input_two = serde_json::json!({
        "prompt": "implement",
        "task_id": "ORB-CONCURRENT-TWO",
        "workspace_path": assigned_two,
        "repo_root": assigned_two,
    });
    let pair_one = validate_declared_worktree_pair(
        &input_one,
        None,
        "run-concurrent-one",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate first pair")
    .expect("first linked pair");
    let pair_two = validate_declared_worktree_pair(
        &input_two,
        None,
        "run-concurrent-two",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate second pair")
    .expect("second linked pair");
    let guard_one = WorktreeBoundaryGuard::capture(
        &input_one,
        None,
        "run-concurrent-one",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair_one),
    )
    .expect("capture first guard")
    .expect("first guard enabled");
    let guard_two = WorktreeBoundaryGuard::capture(
        &input_two,
        None,
        "run-concurrent-two",
        "codex",
        Some(&assigned_two),
        Some(&fixture.primary),
        Some(&pair_two),
    )
    .expect("capture second guard")
    .expect("second guard enabled");

    fs::write(fixture.assigned.join("candidate-one.txt"), "candidate\n")
        .expect("write first candidate");
    fs::write(assigned_two.join("candidate-two.txt"), "candidate\n")
        .expect("write second candidate");
    fs::write(fixture.primary.join("merged-pr.txt"), "merged\n").expect("write merged PR");
    git_ok(&fixture.primary, &["add", "merged-pr.txt"]);
    git_ok(&fixture.primary, &["commit", "-m", "merge concurrent PR"]);

    guard_one
        .verify()
        .expect("first in-flight pipeline accepts the shared base advance");
    guard_two
        .verify()
        .expect("second in-flight pipeline accepts the shared base advance");

    assert!(fixture.assigned.join("candidate-one.txt").exists());
    assert!(assigned_two.join("candidate-two.txt").exists());
}

#[test]
fn concurrent_auto_task_refresh_stays_in_assigned_worktree_during_boundary_checks() {
    let fixture = linked_worktree_fixture();
    let definition_dir = fixture.assigned.join(".orbit/auto_tasks");
    fs::create_dir_all(&definition_dir).expect("definition dir");
    let definition_path = definition_dir.join("doc-duties.yaml");
    fs::write(
        &definition_path,
        "schemaVersion: 1\nname: doc-duties\nrevision: 0\n",
    )
    .expect("seed definition");
    git_ok(
        &fixture.assigned,
        &["add", ".orbit/auto_tasks/doc-duties.yaml"],
    );
    git_ok(
        &fixture.assigned,
        &["commit", "-m", "seed auto-task definition"],
    );
    git_ok(
        &fixture.primary,
        &["merge", "--ff-only", "orbit-integrity-test"],
    );

    let implement_worktree = fixture.root().join("disjoint-implement");
    git_ok(
        &fixture.primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-disjoint-implement",
            implement_worktree
                .to_str()
                .expect("utf8 implement worktree"),
        ],
    );
    let implement_worktree = implement_worktree
        .canonicalize()
        .expect("canonical implement worktree");
    let input_one = worktree_input(&fixture, "ORB-AUTO-TASK-REFRESH");
    let input_two = serde_json::json!({
        "prompt": "implement disjoint work",
        "task_id": "ORB-DISJOINT-IMPLEMENT",
        "workspace_path": implement_worktree,
        "repo_root": implement_worktree,
    });
    let pair_one = validate_declared_worktree_pair(
        &input_one,
        None,
        "run-auto-task-refresh",
        "codex",
        Some(&fixture.primary),
    )
    .expect("validate refresh pair")
    .expect("refresh linked pair");
    let pair_two = validate_declared_worktree_pair(
        &input_two,
        None,
        "run-disjoint-implement",
        "claude",
        Some(&fixture.primary),
    )
    .expect("validate implement pair")
    .expect("implement linked pair");
    let refresh_guard = WorktreeBoundaryGuard::capture(
        &input_one,
        None,
        "run-auto-task-refresh",
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair_one),
    )
    .expect("capture refresh guard")
    .expect("refresh guard enabled");
    let implement_guard = WorktreeBoundaryGuard::capture(
        &input_two,
        None,
        "run-disjoint-implement",
        "claude",
        Some(&implement_worktree),
        Some(&fixture.primary),
        Some(&pair_two),
    )
    .expect("capture implement guard")
    .expect("implement guard enabled");
    let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);

    let (staged_tx, staged_rx) = std::sync::mpsc::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        for revision in 1..=32 {
            let staged = definition_dir.join(format!(".doc-duties.{revision}.tmp"));
            fs::write(
                &staged,
                format!("schemaVersion: 1\nname: doc-duties\nrevision: {revision}\n"),
            )
            .expect("stage definition");
            if revision == 1 {
                staged_tx.send(()).expect("signal staged refresh");
                continue_rx.recv().expect("continue refresh");
            }
            fs::rename(&staged, &definition_path).expect("atomically refresh definition");
        }
    });

    staged_rx.recv().expect("refresh reached staging point");
    implement_guard
        .verify()
        .expect("disjoint boundary snapshot must not report primary checkout drift");
    continue_tx.send(()).expect("release refresh");
    writer.join().expect("refresh thread");
    refresh_guard
        .verify()
        .expect("refresh boundary must remain inside its assigned worktree");

    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_before,
        "concurrent refresh must leave registered primary byte-identical"
    );
    assert!(
        fixture
            .assigned
            .join(".orbit/auto_tasks/doc-duties.yaml")
            .is_file()
    );
}

#[test]
fn failed_auto_task_refresh_preserves_primary_and_audits_definition_and_run() {
    let fixture = linked_worktree_fixture();
    let primary_before = git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nmkdir -p .orbit/auto_tasks\nprintf 'partial\\n' > .orbit/auto_tasks/.doc-duties.tmp\nrm .orbit/auto_tasks/.doc-duties.tmp\nprintf 'auto-task doc-duties refresh failed\\n' >&2\nexit 23\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink.clone();
    let audit = Arc::new(V2AuditWriter::new(
        "run-auto-task-refresh-failed",
        "codex:gpt-5.5",
        sink_for_writer,
    ));

    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-auto-task-refresh-failed",
        audit.clone(),
        &worktree_input(&fixture, "ORB-AUTO-TASK-REFRESH"),
        None,
    )
    .expect("failed provider remains an audited dispatch outcome");

    assert!(!outcome.success);
    assert_eq!(
        git_bytes(&fixture.primary, &["status", "--porcelain=v2", "-z"]),
        primary_before,
        "failed refresh must leave registered primary byte-identical"
    );
    let events = audit.events_snapshot().expect("audit events");
    let failed = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationFinished {
                exit_code,
                stderr_blob_ref,
                ..
            } if *exit_code == Some(23) => Some((
                event.envelope.run_id.as_str(),
                stderr_blob_ref.as_deref().expect("stderr blob"),
            )),
            _ => None,
        })
        .expect("durable failed-refresh event");
    assert_eq!(failed.0, "run-auto-task-refresh-failed");
    assert_eq!(
        sink.blob(failed.1).as_deref(),
        Some(b"auto-task doc-duties refresh failed\n".as_slice()),
        "failure evidence must identify the auto-task"
    );
}

#[test]
fn unchanged_pre_dirty_path_is_excluded_from_escape_diagnostic() {
    let fixture = linked_worktree_fixture();
    fs::write(
        fixture.primary.join("README.md"),
        "pre-existing primary dirtiness\n",
    )
    .expect("dirty primary before capture");
    let escaped = fixture.primary.join("escaped-after-capture.txt");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'escaped\\n' > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-predirty-attribution",
        test_audit("run-predirty-attribution", "codex"),
        &worktree_input(&fixture, "ORB-PREDIRTY-ATTRIBUTION"),
        None,
    )
    .expect_err("new primary delta must fail closed");

    let diagnostic = worktree_integrity_diagnostic(&error);
    assert_eq!(
        diagnostic["changed_paths"],
        serde_json::json!(["escaped-after-capture.txt"]),
        "unchanged pre-existing dirtiness must not be attributed to this invocation"
    );
    assert_eq!(
        diagnostic["primary_before"]["path_states"]["README.md"],
        diagnostic["primary_after"]["path_states"]["README.md"],
        "the diagnostic must retain identical before/after identity for the pre-dirty path"
    );
    assert!(
        diagnostic["primary_after"]["path_states"]["escaped-after-capture.txt"]
            ["untracked_content_sha256"]
            .is_string(),
        "the new untracked path needs its own content identity"
    );
}

#[test]
fn staged_only_primary_delta_reports_its_path_and_index_identity() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'staged-only\\n' > '{}/README.md'\ngit -C '{}' add -- README.md\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
            fixture.primary.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-staged-only-attribution",
        test_audit("run-staged-only-attribution", "codex"),
        &worktree_input(&fixture, "ORB-STAGED-ONLY"),
        None,
    )
    .expect_err("staged primary delta must fail closed");

    let diagnostic = worktree_integrity_diagnostic(&error);
    assert_eq!(
        diagnostic["changed_paths"],
        serde_json::json!(["README.md"])
    );
    let after = &diagnostic["primary_after"]["path_states"]["README.md"];
    assert!(after["index_entry_sha256"].is_string());
    assert!(after["staged_patch_sha256"].is_string());
    assert!(
        after["worktree_patch_sha256"].is_null(),
        "a fully staged edit has no index-to-worktree patch"
    );
    assert_eq!(after["worktree_present"], true);
    assert!(after["untracked_content_sha256"].is_null());
}

#[test]
fn primary_escape_is_typed_non_retryable_and_preserves_both_checkouts() {
    let fixture = linked_worktree_fixture();
    let escaped = fixture.primary.join("escaped.txt");
    let script = fixture.root().join("claude");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'escaped\\n' > '{}'\ngit -C '{}' add -- escaped.txt\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display(),
            fixture.primary.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());
    let audit = test_audit("run-deliberate-escape", "claude");

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec_for("claude", Duration::from_secs(5)),
        "test_activity",
        "run-deliberate-escape",
        audit.clone(),
        &worktree_input(&fixture, "ORB-ESCAPE"),
        None,
    )
    .expect_err("primary write must fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-ESCAPE", "run-deliberate-escape", "claude"),
        &fixture,
        "escaped.txt",
    );
    assert!(error.is_non_retryable());
    assert!(
        escaped.exists(),
        "diagnosis must not clean the primary delta"
    );
    assert!(
        !fixture.assigned.join("escaped.txt").exists(),
        "diagnosis must not copy the primary delta"
    );
    assert_eq!(
        String::from_utf8(git_bytes(
            &fixture.primary,
            &["diff", "--cached", "--name-only", "HEAD", "--"]
        ))
        .expect("utf8 staged paths")
        .trim(),
        "escaped.txt",
        "diagnosis must not reset the provider-mutated primary index"
    );
    assert!(
        audit
            .events_snapshot()
            .expect("audit events")
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::CliInvocationFinished { .. })),
        "terminal provider audit must precede the integrity failure"
    );
}

#[test]
fn primary_content_mutation_is_typed_even_when_assigned_content_also_changes() {
    let fixture = linked_worktree_fixture();
    let escaped = fixture.primary.join("ambiguous-primary.txt");
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned\\n' > ambiguous-assigned.txt\nprintf 'primary\\n' > '{}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            escaped.display()
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-ambiguous-integrity",
        test_audit("run-ambiguous-integrity", "codex"),
        &worktree_input(&fixture, "ORB-AMBIGUOUS"),
        None,
    )
    .expect_err("dual-checkout mutation must fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-AMBIGUOUS", "run-ambiguous-integrity", "codex"),
        &fixture,
        "ambiguous-primary.txt",
    );
    assert!(fixture.assigned.join("ambiguous-assigned.txt").exists());
    assert!(escaped.exists());
}

#[test]
fn dirty_integrity_failure_persists_and_restores_tracked_and_untracked_content() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-RECOVER-DIRTY", "run-recover-dirty");

    fs::write(
        fixture.assigned.join("README.md"),
        b"recover tracked bytes\0\n",
    )
    .expect("write binary tracked candidate");
    let untracked = write_worktree_file(
        &fixture.assigned,
        "nested/untracked.bin",
        "recover untracked bytes\0\n",
    );
    write_primary_file(
        &fixture,
        "src/escaped.rs",
        "fn escaped_primary_write() {}\n",
    );

    let error = guard
        .verify()
        .expect_err("primary escape must fail after preserving assigned dirt");
    let diagnostic = worktree_integrity_diagnostic(&error);
    let recovery = &diagnostic["recovery"];
    let tracked_patch = PathBuf::from(
        recovery["tracked_patch"]
            .as_str()
            .expect("tracked patch path"),
    );
    let untracked_payload = PathBuf::from(
        recovery["untracked_payload"]
            .as_str()
            .expect("untracked payload path"),
    );
    let manifest = PathBuf::from(recovery["manifest"].as_str().expect("manifest path"));

    assert!(
        tracked_patch.is_file(),
        "tracked patch is durable before cleanup"
    );
    assert_eq!(
        fs::read(untracked_payload.join("nested/untracked.bin")).expect("read payload"),
        b"recover untracked bytes\0\n"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(manifest).expect("read manifest"))
            .expect("parse manifest");
    assert_eq!(manifest["runId"], "run-recover-dirty");
    assert_eq!(manifest["taskId"], "ORB-RECOVER-DIRTY");
    assert_eq!(
        manifest["untrackedFiles"],
        serde_json::json!(["nested/untracked.bin"])
    );

    git_ok(&fixture.assigned, &["reset", "--hard", "HEAD"]);
    fs::remove_file(&untracked).expect("simulate cleanup of untracked candidate");
    git_ok(
        &fixture.assigned,
        &[
            "apply",
            "--binary",
            tracked_patch.to_str().expect("utf8 patch path"),
        ],
    );
    fs::create_dir_all(fixture.assigned.join("nested")).expect("restore nested payload parent");
    fs::copy(
        untracked_payload.join("nested/untracked.bin"),
        fixture.assigned.join("nested/untracked.bin"),
    )
    .expect("restore untracked payload");

    assert_eq!(
        fs::read(fixture.assigned.join("README.md")).expect("read restored tracked file"),
        b"recover tracked bytes\0\n"
    );
    assert_eq!(
        fs::read(fixture.assigned.join("nested/untracked.bin"))
            .expect("read restored untracked file"),
        b"recover untracked bytes\0\n"
    );
}

#[test]
fn provider_created_commit_is_a_typed_boundary_failure_without_admission_checks() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-REJECT-COMMIT", "run-reject-commit");

    fs::write(
        fixture.assigned.join("candidate.txt"),
        "provider candidate\n",
    )
    .expect("write provider candidate");
    git_ok(&fixture.assigned, &["add", "--", "candidate.txt"]);
    git_ok(
        &fixture.assigned,
        &[
            "commit",
            "-m",
            "provider commit",
            "-m",
            "Agent-Run: run-reject-commit\nAgent-Task: ORB-REJECT-COMMIT",
        ],
    );

    let error = guard
        .verify()
        .expect_err("provider-created commits are never admissible");
    assert!(matches!(
        error,
        DispatchError::WorktreeIntegrity {
            code: "worktree_content_conflict",
            ..
        }
    ));
    let diagnostic = worktree_integrity_diagnostic(&error);
    let reason = diagnostic["reason"].as_str().expect("typed reason");
    assert!(
        reason.contains("must not create commits or move HEAD"),
        "{reason}"
    );
    assert!(!reason.contains("Agent-Run"), "{reason}");
    assert!(!reason.contains("Agent-Task"), "{reason}");
    assert!(!reason.contains("candidate.txt"), "{reason}");
}

#[test]
fn assigned_history_divergence_is_a_typed_worktree_content_conflict() {
    let fixture = linked_worktree_fixture();
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        "#!/bin/sh\ncat > /dev/null\nprintf 'committed by provider\\n' > assigned-commit.txt\ngit add -- assigned-commit.txt\ngit commit -m assigned-history-change\nprintf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'\n",
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-assigned-history-change",
        test_audit("run-assigned-history-change", "codex"),
        &worktree_input(&fixture, "ORB-ASSIGNED-HISTORY"),
        None,
    )
    .expect_err("provider-created worktree history must fail closed");

    assert_worktree_integrity_error(
        &error,
        "worktree_content_conflict",
        (
            "ORB-ASSIGNED-HISTORY",
            "run-assigned-history-change",
            "codex",
        ),
        &fixture,
        "assigned-commit.txt",
    );
    let diagnostic = worktree_integrity_diagnostic(&error);
    assert_eq!(
        diagnostic["changed_paths"],
        serde_json::json!(["assigned-commit.txt"]),
        "worktree conflicts report the run's paths, not primary checkout paths"
    );
}

#[test]
fn non_fast_forward_primary_move_remains_a_typed_drift_failure() {
    let fixture = linked_worktree_fixture();
    fs::write(fixture.primary.join("before-reset.txt"), "before reset\n")
        .expect("write primary commit");
    git_ok(&fixture.primary, &["add", "before-reset.txt"]);
    git_ok(&fixture.primary, &["commit", "-m", "primary before reset"]);
    let script = fixture.root().join("codex");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\ncat > /dev/null\nprintf 'assigned survives\\n' > assigned-survives.txt\ngit -C '{}' reset --hard HEAD^\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            fixture.primary.display(),
        ),
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(fixture.primary.clone());

    let error = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "run-primary-reset",
        test_audit("run-primary-reset", "codex"),
        &worktree_input(&fixture, "ORB-PRIMARY-RESET"),
        None,
    )
    .expect_err("a primary reset must remain fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-PRIMARY-RESET", "run-primary-reset", "codex"),
        &fixture,
        "before-reset.txt",
    );
    assert!(
        fixture.assigned.join("assigned-survives.txt").exists(),
        "the guard diagnoses without discarding the run candidate"
    );
}

#[test]
fn benign_primary_fast_forward_ignores_primary_dirt_disjoint_from_the_run() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-BENIGN-FF-DIRT", "run-benign-ff-dirt");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    advance_primary(&fixture, "merged-pr.txt");
    let unrelated = fixture.primary.join(".orbit/routines/worktree_gc.yaml");
    fs::create_dir_all(unrelated.parent().expect("routines parent")).expect("create routines dir");
    fs::write(&unrelated, "schemaVersion: 1\n").expect("write unrelated primary dirt");

    let (result, events) = capture_events(|| guard.verify());
    result.expect("an unrelated untracked primary path must not defeat a benign fast-forward");

    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(".orbit/routines/worktree_gc.yaml"))),
        "the accepted fast-forward must report the ignored primary path: {events:?}"
    );
    assert!(
        unrelated.exists(),
        "the guard must not clean the primary dirt it ignored"
    );
}

#[test]
fn dirty_to_clean_primary_fast_forward_is_accepted() {
    // The F2026-07-172 shape: the primary already carries untracked
    // record-store dirt when the guard captures its "before" state, then a
    // fast-forward commit lands exactly that pre-existing dirty content, so
    // `dirty_paths` moves from non-empty to empty even though no interference
    // occurred (HEAD proven to have advanced, `conflicting_paths` empty).
    let fixture = linked_worktree_fixture();
    let dirty_record = ".orbit/auto_tasks/nightly.yaml";
    write_primary_file(&fixture, dirty_record, "name: nightly\n");

    let guard = boundary_guard(&fixture, "ORB-DIRTY-TO-CLEAN-FF", "run-dirty-to-clean-ff");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    let head_before = git_bytes(&fixture.primary, &["rev-parse", "HEAD"]);
    git_ok(&fixture.primary, &["add", "--", dirty_record]);
    git_ok(
        &fixture.primary,
        &[
            "commit",
            "-m",
            "commit exactly the pre-existing dirty content",
        ],
    );

    let (result, events) = capture_events(|| guard.verify());
    result.expect(
        "a fast-forward that lands exactly the primary's pre-existing dirty content must be accepted",
    );

    assert_ne!(
        git_bytes(&fixture.primary, &["rev-parse", "HEAD"]),
        head_before,
        "the accepted case is defined by a proven fast-forward advance"
    );
    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(dirty_record))),
        "the accepted fast-forward must report the dirty-to-clean path: {events:?}"
    );
}

#[test]
fn primary_dirt_intersecting_the_run_defeats_a_fast_forward() {
    for (kind, shared) in [("untracked", "shared-new.txt"), ("tracked", "README.md")] {
        let fixture = linked_worktree_fixture();
        let run_id = format!("run-{kind}-interference");
        let task_id = format!("ORB-{}-INTERFERENCE", kind.to_ascii_uppercase());
        let guard = boundary_guard(&fixture, &task_id, &run_id);

        fs::write(fixture.assigned.join(shared), "run candidate\n").expect("write run candidate");
        advance_primary(&fixture, "merged-pr.txt");
        fs::write(fixture.primary.join(shared), "primary escape\n").expect("write primary escape");

        let error = guard
            .verify()
            .expect_err("primary dirt on a path the run touched must fail closed");

        assert_worktree_integrity_error(
            &error,
            "primary_checkout_drift",
            (&task_id, &run_id, "codex"),
            &fixture,
            shared,
        );
        assert_eq!(
            worktree_integrity_diagnostic(&error)["conflicting_paths"],
            serde_json::json!([shared]),
            "{kind} interference must be named as a conflicting path"
        );
    }
}

#[test]
fn stationary_primary_record_store_dirt_disjoint_from_the_run_is_accepted() {
    let fixture = linked_worktree_fixture();
    let tracked_record = ".orbit/auto_tasks/nightly.yaml";
    write_primary_file(&fixture, tracked_record, "name: nightly\n");
    git_ok(&fixture.primary, &["add", "--", tracked_record]);
    git_ok(&fixture.primary, &["commit", "-m", "track an auto-task"]);

    let guard = boundary_guard(&fixture, "ORB-STATIONARY-DIRT", "run-stationary-dirt");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    // The F2026-07-166 shape: an out-of-run curation pass re-serializes an
    // already-tracked record and drops an untracked sibling, all disjoint from
    // the run, while the primary HEAD and branch never move.
    let head_before = git_bytes(&fixture.primary, &["rev-parse", "HEAD"]);
    write_primary_file(&fixture, tracked_record, "name: nightly\nenabled: true\n");
    let untracked_record = write_primary_file(
        &fixture,
        ".orbit/frictions/F-0002/friction.yaml",
        "id: F-0002\n",
    );

    let (result, events) = capture_events(|| guard.verify());
    result.expect("stationary record-store dirt disjoint from the run must not raise drift");

    assert_eq!(
        git_bytes(&fixture.primary, &["rev-parse", "HEAD"]),
        head_before,
        "the accepted case is defined by an unmoved primary HEAD"
    );
    assert!(
        events.iter().any(|event| event
            .field("ignored_primary_paths")
            .is_some_and(|paths| paths.contains(tracked_record)
                && paths.contains(".orbit/frictions/F-0002/friction.yaml"))),
        "the accepted stationary delta must report both ignored primary paths: {events:?}"
    );
    assert!(
        untracked_record.exists(),
        "the guard must not clean the primary dirt it ignored"
    );
}

#[test]
fn stationary_primary_source_edit_stays_fail_closed_even_when_disjoint() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-STATIONARY-SOURCE", "run-stationary-source");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    // Disjoint from the run, but outside Orbit's record store: this is the
    // ORB-10134 escape shape, and no path-disjointness argument may excuse it.
    write_primary_file(&fixture, ".orbit/routines/nightly.yaml", "name: nightly\n");
    write_primary_file(&fixture, "src/escaped.rs", "fn escaped() {}\n");

    let error = guard
        .verify()
        .expect_err("a primary source edit must remain fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-STATIONARY-SOURCE", "run-stationary-source", "codex"),
        &fixture,
        "src/escaped.rs",
    );
    assert_eq!(
        worktree_integrity_diagnostic(&error)["conflicting_paths"],
        serde_json::json!([]),
        "the source edit is fatal on its path class, not on run interference"
    );
}

#[test]
fn stationary_primary_dirt_intersecting_the_run_remains_a_typed_drift_failure() {
    // Both paths are inside the record store, so only the intersection with
    // `run_changed_paths` can be what keeps them fail-closed.
    for (kind, shared) in [
        ("untracked", ".orbit/frictions/F-0009/friction.yaml"),
        ("tracked", ".orbit/routines/worktree_gc.yaml"),
    ] {
        let fixture = linked_worktree_fixture();
        if kind == "tracked" {
            write_primary_file(&fixture, shared, "schemaVersion: 1\n");
            git_ok(&fixture.primary, &["add", "--", shared]);
            git_ok(&fixture.primary, &["commit", "-m", "track a routine"]);
        }
        let run_id = format!("run-stationary-{kind}-interference");
        let task_id = format!("ORB-STATIONARY-{}", kind.to_ascii_uppercase());
        let guard = boundary_guard(&fixture, &task_id, &run_id);

        write_worktree_file(&fixture.assigned, shared, "run candidate\n");
        write_primary_file(&fixture, shared, "primary escape\n");

        let error = guard
            .verify()
            .expect_err("primary dirt on a path the run touched must fail closed");

        assert_worktree_integrity_error(
            &error,
            "primary_checkout_drift",
            (&task_id, &run_id, "codex"),
            &fixture,
            shared,
        );
        assert_eq!(
            worktree_integrity_diagnostic(&error)["conflicting_paths"],
            serde_json::json!([shared]),
            "stationary {kind} interference must be named as a conflicting path"
        );
    }
}

#[test]
fn primary_branch_switch_remains_a_typed_drift_failure() {
    let fixture = linked_worktree_fixture();
    let guard = boundary_guard(&fixture, "ORB-PRIMARY-BRANCH", "run-primary-branch");

    fs::write(fixture.assigned.join("candidate.txt"), "candidate\n").expect("write run candidate");
    git_ok(
        &fixture.primary,
        &["checkout", "-b", "orbit-primary-switch"],
    );

    let error = guard
        .verify()
        .expect_err("a primary branch switch must remain fail closed");

    assert_worktree_integrity_error(
        &error,
        "primary_checkout_drift",
        ("ORB-PRIMARY-BRANCH", "run-primary-branch", "codex"),
        &fixture,
        "<branch-ref>",
    );
}

#[test]
fn primary_escape_is_checked_after_nonzero_exit_and_timeout() {
    for (terminal, trailer, timeout) in [
        ("nonzero", "exit 23", Duration::from_secs(5)),
        // The timeout arm's budget has to cover a fork/exec plus one write on a
        // host running the rest of the suite alongside it; the deadline starts
        // before the spawn, not when the child reaches its first statement.
        ("timeout", "sleep 10", Duration::from_secs(4)),
    ] {
        let fixture = linked_worktree_fixture();
        let escaped_name = format!("{terminal}-primary.txt");
        let escaped = fixture.primary.join(&escaped_name);
        let script = fixture.root().join("codex");
        // The escape write comes before the stdin drain deliberately. `cat`
        // blocks until the supervisor's detached writer thread runs and drops
        // the pipe, so draining first makes the child's side effect wait on a
        // parent thread the scheduler owes nothing to. On the timeout arm that
        // stall is charged to the deadline: under full-suite load the child was
        // killed having written nothing, leaving no drift to detect and failing
        // the assertion for a reason the boundary guard had no part in.
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\nprintf '{terminal}\\n' > '{}'\ncat > /dev/null\n{trailer}\n",
                escaped.display()
            ),
        );
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(fixture.primary.clone());
        let run_id = format!("run-{terminal}-escape");
        let task_id = format!("ORB-{}", terminal.to_ascii_uppercase());

        let error = run_cli_backend(
            &host,
            &test_agent_loop_spec(timeout),
            "test_activity",
            &run_id,
            test_audit(&run_id, "codex"),
            &worktree_input(&fixture, &task_id),
            None,
        )
        .unwrap_err();

        assert_worktree_integrity_error(
            &error,
            "primary_checkout_drift",
            (&task_id, &run_id, "codex"),
            &fixture,
            &escaped_name,
        );
        assert!(escaped.exists(), "{terminal} delta must remain for rescue");
    }
}

pub(super) struct LinkedWorktreeFixture {
    temp: TempDir,
    pub(super) primary: PathBuf,
    pub(super) assigned: PathBuf,
}

impl LinkedWorktreeFixture {
    pub(super) fn root(&self) -> &Path {
        self.temp.path()
    }
}

pub(super) fn linked_worktree_fixture() -> LinkedWorktreeFixture {
    let temp = tempdir().expect("fixture tempdir");
    let primary = temp.path().join("primary");
    let assigned = temp.path().join("assigned");
    fs::create_dir_all(&primary).expect("create primary");
    git_ok(&primary, &["init"]);
    git_ok(&primary, &["config", "user.name", "Orbit Test"]);
    git_ok(
        &primary,
        &["config", "user.email", "orbit-test@example.invalid"],
    );
    fs::write(primary.join("README.md"), "base\n").expect("write initial file");
    git_ok(&primary, &["add", "README.md"]);
    git_ok(&primary, &["commit", "-m", "initial"]);
    git_ok(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "orbit-integrity-test",
            assigned.to_str().expect("utf8 assigned path"),
        ],
    );

    LinkedWorktreeFixture {
        primary: primary.canonicalize().expect("canonical primary"),
        assigned: assigned.canonicalize().expect("canonical assigned"),
        temp,
    }
}

/// Capture a boundary guard over the fixture's validated linked-worktree pair.
fn boundary_guard(
    fixture: &LinkedWorktreeFixture,
    task_id: &str,
    run_id: &str,
) -> WorktreeBoundaryGuard {
    let input = worktree_input(fixture, task_id);
    let pair =
        validate_declared_worktree_pair(&input, None, run_id, "codex", Some(&fixture.primary))
            .expect("validate declared pair")
            .expect("linked worktree pair");
    WorktreeBoundaryGuard::capture(
        &input,
        None,
        run_id,
        "codex",
        Some(&fixture.assigned),
        Some(&fixture.primary),
        Some(&pair),
    )
    .expect("capture boundary guard")
    .expect("boundary guard enabled")
}

/// Fast-forward the registered primary the way a merged sibling PR does.
fn advance_primary(fixture: &LinkedWorktreeFixture, path: &str) {
    fs::write(fixture.primary.join(path), "merged\n").expect("write merged PR file");
    git_ok(&fixture.primary, &["add", "--", path]);
    git_ok(&fixture.primary, &["commit", "-m", "merge sibling PR"]);
}

/// Write a (possibly nested) path inside the registered primary checkout.
fn write_primary_file(fixture: &LinkedWorktreeFixture, path: &str, contents: &str) -> PathBuf {
    write_worktree_file(&fixture.primary, path, contents)
}

fn write_worktree_file(root: &Path, path: &str, contents: &str) -> PathBuf {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("nested path has a parent"))
        .expect("create parent dirs");
    fs::write(&target, contents).expect("write checkout file");
    target
}

pub(super) fn git_ok(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn git_bytes(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} in {} failed: {}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) fn test_audit(run_id: &str, provider: &str) -> Arc<V2AuditWriter> {
    let sink: Arc<dyn AuditSink> = Arc::new(RecordingSink::default());
    Arc::new(V2AuditWriter::new(
        run_id,
        format!("{provider}:test-model"),
        sink,
    ))
}

fn rendered_implement_input_from_asset(
    yaml: &str,
    assigned_root: &Path,
    task_id: &str,
) -> (String, serde_json::Value) {
    let asset = load_job_asset(yaml).expect("committed shipment asset parses");
    assert!(
        matches!(
            asset.name.as_str(),
            "task_local_pipeline" | "task_pr_pipeline"
        ),
        "unexpected shipment asset {}",
        asset.name
    );
    let implement_bundle = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "implement_bundle")
        .expect("shipment asset has implement_bundle");
    let JobV2StepBody::Loop { loop_ } = &implement_bundle.body else {
        panic!("{} implement_bundle must be a loop", asset.name);
    };
    let implement_one = loop_
        .steps
        .iter()
        .find(|step| step.id == "implement_one")
        .expect("shipment asset has implement_one");
    let JobV2StepBody::TargetRef(implement) = &implement_one.body else {
        panic!("{} implement_one must reference an activity", asset.name);
    };
    assert_eq!(implement.target, "activity:agent_implement");
    let template_input = implement
        .default_input
        .as_ref()
        .expect("agent_implement default input");
    for field in ["workspace_path", "repo_root"] {
        assert_eq!(
            template_input[field], "{{ steps.worktree.output.workspace_path }}",
            "{} must source {field} from the committed worktree output",
            asset.name
        );
    }

    let assigned = assigned_root.display().to_string();
    let mut steps = HashMap::new();
    steps.insert(
        "worktree".to_string(),
        serde_json::json!({ "output": { "workspace_path": assigned } }),
    );
    let context = TemplateContext {
        item: Some(serde_json::json!(task_id)),
        steps,
        ..TemplateContext::default()
    };
    let rendered = render_asset_value(template_input, &context);
    assert_eq!(rendered["task_id"], task_id);
    assert_eq!(rendered["workspace_path"], assigned);
    assert_eq!(rendered["repo_root"], assigned);
    (asset.name, rendered)
}

fn rendered_finish_input_from_epic_pipeline(
    assigned_root: &Path,
    task_id: &str,
) -> serde_json::Value {
    let asset = load_job_asset(EPIC_PIPELINE_YAML).expect("epic pipeline parses");
    let assemble = asset
        .spec
        .steps
        .iter()
        .find(|step| step.id == "assemble")
        .expect("epic pipeline has assemble");
    let JobV2StepBody::Loop { loop_ } = &assemble.body else {
        panic!("epic assemble must be a loop");
    };
    let finish = loop_
        .steps
        .iter()
        .find(|step| step.id == "finish")
        .expect("epic assemble has finish");
    let JobV2StepBody::TargetRef(finish) = &finish.body else {
        panic!("epic finish must reference epic_orchestrator");
    };
    assert_eq!(finish.target, "activity:epic_orchestrator");
    let template_input = finish
        .default_input
        .as_ref()
        .expect("epic_orchestrator default input");
    for field in ["workspace_path", "repo_root"] {
        assert_eq!(
            template_input[field], "{{ steps.worktree.output.workspace_path }}",
            "epic finisher must source {field} from the assigned worktree"
        );
    }

    let assigned = assigned_root.display().to_string();
    let mut steps = HashMap::new();
    steps.insert(
        "worktree".to_string(),
        serde_json::json!({ "output": { "workspace_path": assigned } }),
    );
    let context = TemplateContext {
        input: serde_json::json!({ "epic_task_id": task_id }),
        steps,
        ..TemplateContext::default()
    };
    let rendered = render_asset_value(template_input, &context);
    assert_eq!(rendered["task_id"], task_id);
    assert_eq!(rendered["workspace_path"], assigned);
    assert_eq!(rendered["repo_root"], assigned);
    rendered
}

fn epic_orchestrator_spec(timeout: Duration) -> orbit_types::workflow::activity_job::AgentLoopSpec {
    let asset = load_activity_asset(EPIC_ORCHESTRATOR_YAML).expect("parse epic orchestrator");
    let ActivityV2Spec::AgentLoop(mut spec) = asset.spec.spec else {
        panic!("epic_orchestrator must be an agent_loop");
    };
    spec.wall_clock_timeout_seconds = timeout.as_secs();
    spec.provider = orbit_types::workflow::activity_job::Provider::Codex;
    spec
}

fn render_asset_value(value: &serde_json::Value, context: &TemplateContext) -> serde_json::Value {
    match value {
        serde_json::Value::String(template_value) if template_value.contains("{{") => {
            serde_json::Value::String(
                template::render(template_value, context).expect("render committed asset input"),
            )
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| render_asset_value(value, context))
                .collect(),
        ),
        serde_json::Value::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), render_asset_value(value, context)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn worktree_input(fixture: &LinkedWorktreeFixture, task_id: &str) -> serde_json::Value {
    serde_json::json!({
        "prompt": "implement",
        "task_id": task_id,
        "workspace_path": fixture.assigned,
        "repo_root": fixture.assigned,
    })
}

fn worktree_integrity_diagnostic(error: &DispatchError) -> serde_json::Value {
    let DispatchError::WorktreeIntegrity { diagnostic, .. } = error else {
        panic!("expected WorktreeIntegrity, got {error:?}");
    };
    serde_json::from_str(diagnostic).expect("worktree integrity diagnostic is JSON")
}

fn assert_worktree_mismatch(
    error: &DispatchError,
    task_id: &str,
    run_id: &str,
    reason_fragment: &str,
) {
    assert!(
        matches!(
            error,
            DispatchError::WorktreeIntegrity {
                code: "worktree_mismatch",
                ..
            }
        ),
        "unexpected mismatch error: {error:?}"
    );
    assert!(error.is_non_retryable());
    let diagnostic = worktree_integrity_diagnostic(error);
    assert_eq!(diagnostic["code"], "worktree_mismatch");
    assert_eq!(diagnostic["task_id"], task_id);
    assert_eq!(diagnostic["run_id"], run_id);
    assert_eq!(diagnostic["provider"], "codex");
    assert!(
        diagnostic["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(reason_fragment)),
        "unexpected mismatch reason: {}",
        diagnostic["reason"]
    );
}

fn assert_pre_spawn_failure(audit: &V2AuditWriter, marker: &Path) {
    assert!(!marker.exists(), "provider process must not have started");
    assert!(
        !audit
            .events_snapshot()
            .expect("audit events")
            .iter()
            .any(|event| matches!(event.kind, V2AuditEventKind::CliInvocationStarted { .. })),
        "mismatch must be rejected before cli.invocation.started"
    );
}

fn assert_worktree_integrity_error(
    error: &DispatchError,
    expected_code: &str,
    identity: (&str, &str, &str),
    fixture: &LinkedWorktreeFixture,
    changed_path: &str,
) {
    let (task_id, run_id, provider) = identity;
    assert!(
        matches!(
            error,
            DispatchError::WorktreeIntegrity { code, .. } if *code == expected_code
        ),
        "unexpected integrity error: {error:?}"
    );
    let rendered = error.to_string();
    for expected in [
        expected_code,
        task_id,
        run_id,
        provider,
        &fixture.assigned.display().to_string(),
        &fixture.primary.display().to_string(),
        changed_path,
        "assigned_before",
        "assigned_after",
        "primary_before",
        "primary_after",
    ] {
        assert!(
            rendered.contains(expected),
            "integrity diagnostic missing {expected:?}: {rendered}"
        );
    }
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

#[test]
fn run_cli_backend_exports_runtime_identity_for_subprocess_tools() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_AGENT_NAME" = "grok" ] && [ "$ORBIT_AGENT_MODEL" = "grok-build" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"identity_env_missing","message":"runtime identity env was not propagated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-identity-env",
        "grok:grok-build",
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
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-identity-env",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

/// ORB-10342: pipeline-gate provider spawns must carry the same
/// AGENT_RUN_ID/AGENT_MODEL/AGENT_TASK trio the worker path sets
/// (ORB-10340), so the shared prepare-commit-msg injector can stamp commit
/// trailers regardless of which spawner produced the commit.
#[test]
fn run_cli_backend_sets_agent_telemetry_env_vars_for_commit_trailers() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$AGENT_RUN_ID" = "job-grok-telemetry" ] && [ "$AGENT_MODEL" = "grok-build" ] && [ "$AGENT_TASK" = "ORB-10342" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"telemetry_env_missing","message":"agent telemetry env was not propagated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-telemetry",
        "grok:grok-build",
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
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-telemetry",
        audit,
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-10342"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

/// [ORB-10917] End-to-end guard for the composed dispatch environment: a
/// benignly named ambient credential must not survive into the provider child.
/// The ambient value is set by this test rather than inherited from the
/// developer's shell, and the child reports what it actually saw so a
/// regression fails loudly instead of silently forwarding.
#[test]
fn run_cli_backend_does_not_forward_benignly_named_ambient_credentials() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ -z "$DATABASE_URL" ] && [ -z "$BILLING_ENDPOINT" ] && [ -n "$PATH" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '{"schemaVersion":1,"status":"failed","error":{"code":"ambient_env_leaked","message":"DATABASE_URL=%s BILLING_ENDPOINT=%s","details":null}}\n' "$DATABASE_URL" "$BILLING_ENDPOINT"
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-env-allowlist",
        "grok:grok-build",
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
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let _ambient = orbit_common::test_env::scoped([
        ("DATABASE_URL", Some("postgres://svc:hunter2@db.internal")),
        ("BILLING_ENDPOINT", Some("https://billing.internal.example")),
    ]);
    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-env-allowlist",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "ambient credentials leaked into the provider child: {:?}",
        outcome.output
    );
}

/// [ORB-11607] An operator override in the dispatching process must not reach
/// the untrusted provider child. The `ORBIT_` envelope is an explicit name
/// set, not a prefix wildcard, so `ORBIT_OPERATOR` stays with the parent.
#[test]
fn run_cli_backend_does_not_forward_ambient_operator_override() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ -z "${ORBIT_OPERATOR+x}" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '{"schemaVersion":1,"status":"failed","error":{"code":"operator_env_leaked","message":"ORBIT_OPERATOR=%s","details":null}}\n' "$ORBIT_OPERATOR"
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-operator-env-deny",
        "grok:grok-build",
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
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let _ambient = orbit_common::test_env::scoped([("ORBIT_OPERATOR", Some("1"))]);
    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-operator-env-deny",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(
        outcome.success,
        "ORBIT_OPERATOR leaked into the provider child: {:?}",
        outcome.output
    );
}

/// [ORB-10909] CLI-runner dispatch must inject the registry locator from the host so a
/// spawned agent whose HOME does not contain the Orbit registry can still
/// resolve `orbit tool run` against the dispatching run's root.
#[test]
fn run_cli_backend_injects_managed_registry_root_from_host() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("grok");
    write_executable(
        &script,
        r#"#!/bin/sh
cat > /dev/null
if [ "$ORBIT_REGISTRY_ROOT" = "/resolved/orbit/root" ] && [ -z "$ORBIT_ROOT" ] && [ "$ORBIT_WORKSPACE" = "ws_orbit" ]; then
  printf '%s\n' '{"schemaVersion":1,"status":"success","result":{"identity":"ok"},"error":null}'
else
  printf '%s\n' '{"schemaVersion":1,"status":"failed","error":{"code":"registry_root_missing","message":"managed registry routing was not isolated","details":null}}'
  exit 1
fi
"#,
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-orbit-root",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: None,
        workspace_root: None,
        orbit_registry_root: Some("/resolved/orbit/root".to_string()),
        orbit_workspace_selector: Some("ws_orbit".to_string()),
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-orbit-root",
        audit,
        &serde_json::json!({"prompt": "hi"}),
        None,
    )
    .expect("run succeeds");

    assert!(outcome.success);
    assert_eq!(outcome.output["provider"], "grok");
}

/// [ORB-10980] A managed run executes in a linked worktree whose workspace and
/// worktree-local `.orbit` state roots are mounted read-only. The child must be
/// routed to the authoritative registry root the host reports without turning
/// that locator into an explicit workspace/data-root pin. The existing
/// binary/PATH pinning plus managed provenance bindings must survive alongside
/// it — those are what let the documented `orbit tool run` fallback work.
#[test]
fn run_cli_backend_injects_registry_root_not_worktree_state_root() {
    let temp = tempdir().expect("tempdir");
    let registry_root = temp.path().join("registry");
    let workspace_state_root = temp.path().join("repo").join(".orbit");
    let worktree_state_root = temp
        .path()
        .join("repo/.orbit/state/worktrees/jrun-fixture")
        .join(".orbit");
    for directory in [&registry_root, &workspace_state_root, &worktree_state_root] {
        std::fs::create_dir_all(directory).expect("state root");
    }
    // Both `.orbit` state roots are read-only in a managed run; a child pinned
    // to either cannot bootstrap, so make that concrete in the fixture.
    for directory in [&workspace_state_root, &worktree_state_root] {
        let mut permissions = std::fs::metadata(directory)
            .expect("state root metadata")
            .permissions();
        permissions.set_mode(0o555);
        std::fs::set_permissions(directory, permissions).expect("read-only state root");
    }

    let script = temp.path().join("grok");
    write_executable(
        &script,
        &format!(
            r#"#!/bin/sh
cat > /dev/null
fail() {{
  printf '%s\n' "{{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{{\"code\":\"$1\",\"message\":\"$1\",\"details\":null}}}}"
  exit 1
}}
[ "$ORBIT_REGISTRY_ROOT" = "{registry}" ] || fail registry_root_not_authoritative
[ -z "$ORBIT_ROOT" ] || fail operator_root_leaked_into_managed_child
[ "$ORBIT_WORKSPACE" = "ws_orbit" ] || fail workspace_selector_missing
[ "$ORBIT_WORKSPACE" = "daniel-e9c542" ] && fail workspace_selector_is_worktree_identity
[ "$ORBIT_REGISTRY_ROOT" = "{workspace_state}" ] && fail registry_root_is_workspace_state_root
[ "$ORBIT_REGISTRY_ROOT" = "{worktree_state}" ] && fail registry_root_is_worktree_state_root
[ -n "$ORBIT_BIN" ] || fail orbit_bin_missing
[ -n "$PATH" ] || fail path_missing
[ "$ORBIT_RUN_ID" = "job-grok-registry-root" ] || fail run_id_missing
[ "$ORBIT_MANAGED_RUN_CONTEXT" = "1" ] || fail managed_run_context_missing
[ "$ORBIT_TASK_ACTOR_KIND" = "agent" ] || fail actor_kind_missing
[ "$ORBIT_ACTIVITY_TOOLS" = "orbit.task.show,proc.spawn,github.run.list" ] || fail activity_tools_missing
[ "$ORBIT_PROC_ALLOWED_PROGRAMS" = "git,rg" ] || fail proc_programs_missing
[ "$ORBIT_ACTIVITY_FS_PROFILE" = "unrestricted" ] || fail fs_profile_missing
[ "$ORBIT_ACTIVE_TASK_ID" = "ORB-10980" ] || fail active_task_missing
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"identity":"ok"}},"error":null}}'
"#,
            registry = registry_root.display(),
            workspace_state = workspace_state_root.display(),
            worktree_state = worktree_state_root.display(),
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-grok-registry-root",
        "grok:grok-build",
        sink_for_writer,
    ));
    let host = TestHost {
        command: script.display().to_string(),
        executor_args: Vec::new(),
        provider_config: HashMap::new(),
        sandbox: None,
        task_context: Some(serde_json::json!({
            "id": "ORB-10980",
            "required_tools": ["github.run.list"]
        })),
        workspace_root: None,
        orbit_registry_root: Some(registry_root.display().to_string()),
        orbit_workspace_selector: Some("ws_orbit".to_string()),
    };
    let mut spec = test_agent_loop_spec_for("grok", Duration::from_secs(5));
    spec.model = Some("grok-build".to_string());
    spec.tools = vec!["orbit.task.show".to_string(), "proc.spawn".to_string()];
    spec.proc_allowed_programs = Some(vec!["git".to_string(), "rg".to_string()]);
    let ambient_root = registry_root.to_string_lossy().into_owned();
    let _ambient = orbit_common::test_env::scoped([("ORBIT_ROOT", Some(ambient_root.as_str()))]);

    let outcome = run_cli_backend(
        &host,
        &spec,
        "test_activity",
        "job-grok-registry-root",
        audit.clone(),
        &serde_json::json!({"prompt": "hi", "task_id": "ORB-10980"}),
        None,
    )
    .expect("run succeeds");

    // Restore write permission so the fixture's temp tree can be reclaimed.
    for directory in [&workspace_state_root, &worktree_state_root] {
        let mut permissions = std::fs::metadata(directory)
            .expect("state root metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(directory, permissions).expect("restore state root");
    }

    assert!(
        outcome.success,
        "managed child rejected its Orbit environment: {:?}",
        outcome.output
    );
    let events = audit.events_snapshot().expect("audit snapshot");
    let delegated = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::ToolAllowlistHarnessDelegated {
                task_id,
                task_ids,
                requested_tools,
                effective_tools,
                tools,
                ..
            } => Some((task_id, task_ids, requested_tools, effective_tools, tools)),
            _ => None,
        })
        .expect("tool allowlist audit event");
    assert_eq!(delegated.0.as_deref(), Some("ORB-10980"));
    assert_eq!(delegated.1, &["ORB-10980"]);
    assert_eq!(delegated.2, &["github.run.list"]);
    assert_eq!(
        delegated.3,
        &["orbit.task.show", "proc.spawn", "github.run.list"]
    );
    assert_eq!(delegated.4, delegated.3);
}

/// AGENT_MODEL/AGENT_TASK must be omitted (unset), not set to an empty
/// string, when the model or task id is unknown — mirrors ORB-10340's
/// worker-side semantics. AGENT_RUN_ID is always known for a dispatched run.
// Asserted at the builder boundary, not through a spawned child: the composed child env
// forwards the named `ORBIT_*` envelope, so an Orbit-dispatched test run's own run/task
// identity can reach the child and mask a correct omission. The positive propagation case
// above still covers the wiring end-to-end.
#[test]
fn run_cli_backend_omits_agent_model_and_task_env_vars_when_unknown() {
    let vars = provenance_env(ProvenanceEnv {
        orbit_run_id: Some("job-grok-telemetry-unknown"),
        orbit_managed_run_context: true,
        orbit_agent_name: Some("grok"),
        agent_run_id: Some("job-grok-telemetry-unknown"),
        ..ProvenanceEnv::default()
    });

    assert!(vars.contains(&(
        "AGENT_RUN_ID".to_string(),
        "job-grok-telemetry-unknown".to_string()
    )));
    assert!(!vars.iter().any(|(key, _)| key == "AGENT_MODEL"));
    assert!(!vars.iter().any(|(key, _)| key == "AGENT_TASK"));
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this test uses a dedicated variable name and restores the
        // previous value on drop.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see EnvVarGuard::set.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
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
