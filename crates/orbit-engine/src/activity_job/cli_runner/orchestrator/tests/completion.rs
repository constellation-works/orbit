#![allow(missing_docs)]

#[cfg(target_os = "linux")]
use std::fs;
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::super::super::super::dispatcher::ResolvedSandbox;
use super::super::super::tests::cli_run::CliRun;
#[cfg(target_os = "linux")]
use super::super::super::tests::cli_run::{TOUCH_ORBIT_UNGRANTED, TOUCH_ORBIT_UNGRANTED_EXIT_0};

#[test]
fn run_cli_backend_redacts_secret_like_stdout_text_preview() {
    let mut out = CliRun::new()
        .events([
            r#"{"log":"Authorization: Bearer stdout-secret-token"}"#,
            r#"{"x-api-key":"stdout-header-key"}"#,
            r#"{"log":"sk-stdoutsecret123"}"#,
            r#"{"api_key":"stdout-json-key"}"#,
        ])
        .success_envelope()
        .run_id("job-stdout-redaction")
        .audit_agent("codex:gpt-5.5")
        .input(serde_json::json!({"prompt": "hi"}))
        .run();

    let outcome = out.take_result().expect("run succeeds");
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

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires unprivileged user namespaces; bwrap cannot nest inside Orbit's sandbox"]
fn linux_bwrap_failed_invocation_names_ungranted_write_path_and_deny() {
    let cli = CliRun::new()
        .run_id("job-linux-write-denial")
        .audit_agent("codex:test");
    let workspace = cli.root().join("worktree");
    let orbit = workspace.join(".orbit");
    fs::create_dir_all(&orbit).expect("denied Orbit root");
    let blocked_path = orbit.join("ungranted");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "implementer".to_string(),
        read: vec![format!("{}/**", workspace.display())],
        modify: vec![
            format!("{}/**", workspace.display()),
            format!("!{}/**", orbit.display()),
        ],
    };

    let mut out = cli
        .command_path(workspace.join("codex"))
        .script(TOUCH_ORBIT_UNGRANTED)
        .sandbox(ResolvedSandbox {
            kind: orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap,
            fs_profile: profile,
            allow_fallback: false,
            managed_worktree: true,
            runtime_write_authority: Vec::new(),
            mask: None,
        })
        .task_context(serde_json::json!({
            "workspace_path": workspace.display().to_string()
        }))
        .input(serde_json::json!({"prompt": "attempt the write"}))
        .run();

    let outcome = out
        .take_result()
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
    let cli = CliRun::new()
        .run_id("job-linux-exit-zero-denial")
        .audit_agent("claude:test");
    let workspace = cli.root().join("worktree");
    let orbit = workspace.join(".orbit");
    fs::create_dir_all(&orbit).expect("denied Orbit root");
    let blocked_path = orbit.join("ungranted");
    let profile = orbit_types::policy::ResolvedFsProfile {
        name: "unrestricted".to_string(),
        read: vec![format!("{}/**", workspace.display())],
        modify: vec![
            format!("{}/**", workspace.display()),
            format!("!{}/**", orbit.display()),
        ],
    };

    let mut out = cli
        .command_path(workspace.join("codex"))
        .script(TOUCH_ORBIT_UNGRANTED_EXIT_0)
        .sandbox(ResolvedSandbox {
            kind: orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap,
            fs_profile: profile,
            allow_fallback: false,
            managed_worktree: true,
            runtime_write_authority: Vec::new(),
            mask: None,
        })
        .task_context(serde_json::json!({
            "workspace_path": workspace.display().to_string()
        }))
        .input(serde_json::json!({"prompt": "attempt the write"}))
        .run();

    let outcome = out
        .take_result()
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
fn run_cli_backend_uses_grok_final_text_not_wrapper_metadata() {
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"final-text\"},\"error\":null}",
        "stopReason": "EndTurn",
        "thought": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"metadata\",\"message\":\"ignore\",\"details\":null}}",
        "toolCalls": [{"result": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"tool\",\"message\":\"ignore\",\"details\":null}}"}],
    })
    .to_string();
    let mut out = CliRun::new()
        .provider("grok")
        .events([grok_stdout])
        .run_id("job-grok-final-text")
        .audit_agent("grok:grok-build")
        .require_completion_envelope(true)
        .input(serde_json::json!({"prompt": "respond"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["source"], "final-text");
    assert_eq!(outcome.output["response_envelope_status"], "success");
}

#[test]
fn run_cli_backend_preserves_grok_failed_final_text() {
    let grok_stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"failed\",\"result\":{},\"error\":{\"code\":\"final_failure\",\"message\":\"final answer failed\",\"details\":null}}",
        "stopReason": "EndTurn",
        "thought": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"metadata\"},\"error\":null}",
    })
    .to_string();
    let mut out = CliRun::new()
        .provider("grok")
        .events([grok_stdout])
        .run_id("job-grok-final-failure")
        .audit_agent("grok:grok-build")
        .require_completion_envelope(true)
        .input(serde_json::json!({"prompt": "respond"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
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
    // The agent config layer infers provider from the command basename, so
    // the script name must match a known provider. The demotion logic is
    // provider-agnostic — codex exercises the same code path as claude.
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
    let mut out = CliRun::new()
        .events([stdout])
        .run_id("job-success-demote")
        .audit_agent("claude:s")
        .require_response_envelope(true)
        .input(serde_json::json!({"prompt": "hi"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
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
    let mut out = CliRun::new()
        .success_envelope_result_only()
        .run_id("job-success-keep")
        .audit_agent("claude:s")
        .input(serde_json::json!({"prompt": "hi"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
    assert!(
        outcome.success,
        "envelope status=success must keep dispatch success on exit 0"
    );
}

#[test]
fn run_cli_backend_surfaces_antigravity_timeout_terminal_error_when_stderr_empty() {
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "status": "ERROR",
            "response": "secret-transcript should not appear in diagnostics",
            "error": "timeout waiting for response"
        }
    })
    .to_string();
    let mut out = CliRun::new()
        .provider("antigravity")
        .command_name("agy")
        .events([stdout])
        .exit_code(1)
        .run_id("job-agy-timeout-error")
        .audit_agent("antigravity:gemini-3.8-flash-high")
        .timeout(Duration::from_secs(60))
        .input(serde_json::json!({"prompt": "do it"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
    assert!(!outcome.success);
    assert_eq!(outcome.output["timed_out"], false);
    assert_eq!(outcome.output["exit_code"], 1);
    let message = outcome.message.expect("provider diagnostic");
    assert!(
        message.contains("timeout waiting for response"),
        "{message}"
    );
    assert!(
        message.contains("cli subprocess exited with code 1"),
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
    let mut out = CliRun::new()
        .provider("claude")
        .events([stdout])
        .run_id("job-max-turns")
        .audit_agent("claude:sonnet")
        .input(serde_json::json!({"task_id": "ORB-10746"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
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
    let mut out = CliRun::new()
        .provider("claude")
        .stderr_events(["error: unknown option '--json-schema'"])
        .exit_code(1)
        .run_id("job-missing-flag")
        .audit_agent("claude:sonnet")
        .input(serde_json::json!({"task_id": "ORB-10746"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
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
    let stdout = serde_json::json!({
        "is_error": true,
        "subtype": "success",
        "structured_output": Option::<String>::None,
        "result": "API Error: 400 tools.0.custom.input_schema: input_schema does not support \
                   oneOf, allOf, or anyOf at the top level"
    })
    .to_string();
    let mut out = CliRun::new()
        .provider("claude")
        .stdout_exact(&stdout)
        .exit_code(1)
        .run_id("job-rejected-schema")
        .audit_agent("claude:sonnet")
        .input(serde_json::json!({"task_id": "ORB-10746"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
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
    let mut out = CliRun::new()
        .provider("claude")
        .stdout_exact(&stdout)
        .run_id("job-structured-output")
        .audit_agent("claude:sonnet")
        .input(serde_json::json!({"task_id": "ORB-10734"}))
        .run();

    let outcome = out.take_result().expect("run cli backend");
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(outcome.output["completion_envelope_satisfied"], true);
    assert_eq!(outcome.output["response_envelope_status"], "success");
}
