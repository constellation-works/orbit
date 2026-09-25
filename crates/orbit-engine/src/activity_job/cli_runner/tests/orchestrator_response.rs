#![allow(missing_docs)]

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use orbit_agent::loop_engine::audit::AuditSink;
use tempfile::tempdir;

use super::super::super::audit_writer::V2AuditWriter;
use super::super::run_cli_backend;
use super::test_support::{
    RecordingSink, TestHost, capture_events, sandbox_for_test, sh_args, test_agent_loop_spec,
    test_agent_loop_spec_for, write_executable,
};

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
    let launcher = temp.path().join("copilot");
    std::os::unix::fs::symlink("/bin/sh", &launcher).expect("link stable shell as copilot");

    for status in ["failed", "timeout"] {
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
        // Preserve the copilot launcher name while executing a stable shell:
        // freshly written scripts can race inherited writable descriptors.
        let mut host = TestHost::with_command(launcher.display().to_string());
        host.executor_args = sh_args(&format!(
            "cat > /dev/null\nprintf '%s\\n' '{commentary}'\nprintf '%s\\n' '{terminal}'\n"
        ));

        let sink = Arc::new(RecordingSink::default());
        let sink_for_writer: Arc<dyn AuditSink> = sink;
        let audit = Arc::new(V2AuditWriter::new(
            format!("job-copilot-{status}-after-commentary"),
            "copilot:gpt-5.5",
            sink_for_writer,
        ));
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
///
/// The diagnosis depends on the fixture actually running under the compiled
/// profile. On a macOS host where `sandbox-exec` cannot apply a profile at all
/// (an already-confined Orbit process), the wrapper exits 71 before Copilot
/// starts and there is no auth marker to diagnose, so this skips on the same
/// can-apply probe orbit-exec's own sandbox tests use rather than asserting a
/// keychain message for a sandbox that never applied. [DANI-10509]
#[test]
fn run_cli_backend_copilot_keychain_auth_failure_reaches_the_step_message() {
    #[cfg(target_os = "macos")]
    {
        if !super::test_support::sandbox_exec_can_apply_for_test() {
            return;
        }
    }

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
        "copilot:claude-sonnet-5",
        sink_for_writer,
    ));
    let mut sandbox = sandbox_for_test();
    // Linux CI has no sandbox-exec binary at all, so the fixture runs bare and
    // still exercises the diagnostic, which reads the compiled profile. On
    // macOS the probe above already established that the wrapper applies, so
    // this flag changes nothing there.
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
        message.contains("$HOME/Library/Keychains") && message.contains("copilot"),
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
        note.contains("$HOME/Library/Keychains") && note.contains("copilot"),
        "workflow_run_failed must inline the diagnosis: {note}"
    );
}

/// `sandbox-exec` exits 71 (`EX_OSERR`) with
/// `sandbox_apply: Operation not permitted` when the kernel refuses to apply
/// the profile — an already-confined Orbit process, or one without the
/// entitlement. The provider binary never starts, so this is a host condition
/// shared by every provider and has nothing to do with a credential store.
/// Every provider must get the sandbox-application diagnosis and its remedy,
/// and none may be told to inspect Keychain access. [DANI-10509]
#[test]
fn run_cli_backend_names_a_sandbox_application_failure_for_every_provider() {
    for provider in [
        orbit_types::workflow::activity_job::Provider::Claude,
        orbit_types::workflow::activity_job::Provider::Codex,
        orbit_types::workflow::activity_job::Provider::Copilot,
        orbit_types::workflow::activity_job::Provider::Gemini,
    ] {
        let temp = tempdir().expect("tempdir");
        let script = temp.path().join(provider.as_str());
        // Stands in for the wrapper's own failure: on a host where
        // `sandbox-exec` cannot apply, this is verbatim what the step captures
        // before the provider is reached.
        write_executable(
            &script,
            concat!(
                "#!/bin/sh\ncat > /dev/null\n",
                "printf '%s\\n' 'sandbox-exec: sandbox_apply: Operation not permitted' >&2\n",
                "exit 71\n",
            ),
        );

        let sink = Arc::new(RecordingSink::default());
        let sink_for_writer: Arc<dyn AuditSink> = sink;
        let audit = Arc::new(V2AuditWriter::new(
            "job-sandbox-apply",
            provider.as_str(),
            sink_for_writer,
        ));
        let mut sandbox = sandbox_for_test();
        // Linux CI has no sandbox-exec binary, so the fixture stands in for the
        // wrapper there; the diagnosis keys on the captured stderr either way.
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
        spec.provider = provider;

        let outcome = run_cli_backend(
            &host,
            &spec,
            "implement_one",
            "jrun-sandbox-apply",
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
            message.contains("sandbox-exec could not apply")
                && message.contains("sandbox_apply: Operation not permitted"),
            "the step message must name the sandbox-application failure: {message}"
        );
        assert!(
            message.contains("Run Orbit outside the enclosing sandbox")
                && message.contains("`sandbox: off`"),
            "the step message must name the remedy: {message}"
        );
        assert!(
            !message.to_lowercase().contains("keychain"),
            "a wrapper that never applied says nothing about Keychain access: {message}"
        );
    }
}

/// The same exit code without the wrapper's marker is an ordinary provider
/// failure: exit 71 alone must not be read as a sandbox-application failure.
///
/// This needs the fixture's own stderr to be what the step captures, so it
/// skips on a macOS host where `sandbox-exec` cannot apply — there the wrapper
/// exits 71 with its own marker and the provider never runs, which is the
/// condition the sibling test covers. [DANI-10509]
#[test]
fn run_cli_backend_leaves_a_bare_exit_71_undiagnosed() {
    #[cfg(target_os = "macos")]
    {
        if !super::test_support::sandbox_exec_can_apply_for_test() {
            return;
        }
    }

    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' 'copilot: internal service error' >&2\n",
            "exit 71\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-bare-71",
        "copilot:claude-sonnet-5",
        sink_for_writer,
    ));
    let mut sandbox = sandbox_for_test();
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
        "jrun-bare-71",
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
        !message.contains("sandbox-exec could not apply") && !message.contains("Keychain"),
        "exit 71 without the wrapper marker must not be diagnosed: {message}"
    );
}

#[test]
fn run_cli_backend_copilot_unavailable_model_reaches_workflow_failure_note() {
    let temp = tempdir().expect("tempdir");
    let script = temp.path().join("copilot");
    write_executable(
        &script,
        concat!(
            "#!/bin/sh\ncat > /dev/null\n",
            "printf '%s\\n' 'Error: Model \"retired-sonnet\" from --model flag is not available.' >&2\n",
            "exit 1\n",
        ),
    );

    let sink = Arc::new(RecordingSink::default());
    let sink_for_writer: Arc<dyn AuditSink> = sink;
    let audit = Arc::new(V2AuditWriter::new(
        "job-copilot-model",
        "copilot:retired-sonnet",
        sink_for_writer,
    ));
    let host = TestHost::with_command(script.display().to_string());
    let mut spec = test_agent_loop_spec(Duration::from_secs(5));
    spec.provider = orbit_types::workflow::activity_job::Provider::Copilot;

    let outcome = run_cli_backend(
        &host,
        &spec,
        "implement_one",
        "jrun-copilot-model",
        audit,
        &serde_json::json!({"prompt": "say ok", "crew": "nightly"}),
        None,
    )
    .expect("the invocation outcome should be classified");

    assert!(!outcome.success);
    let message = outcome
        .message
        .as_deref()
        .expect("failed step carries a message");
    for expected in ["retired-sonnet", "crew `nightly`", "`crews.nightly.model`"] {
        assert!(
            message.contains(expected),
            "step message must contain {expected:?}: {message}"
        );
    }

    let update = crate::context::blocked_workflow_failure_update(
        "task_pr_pipeline",
        "jrun-copilot-model",
        Some("AGENT_INVOCATION_FAILED"),
        Some(message),
    );
    let note = update
        .status_note
        .as_deref()
        .expect("blocked update carries a note");
    for expected in ["retired-sonnet", "crew `nightly`", "`crews.nightly.model`"] {
        assert!(
            note.contains(expected),
            "workflow_run_failed note must contain {expected:?}: {note}"
        );
    }
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
