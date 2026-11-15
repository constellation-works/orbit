//! Generic subprocess failures retain bounded operator evidence without
//! letting untrusted stderr change incident or workflow classification.

use super::*;
use orbit_store::contracts::group_failure_incidents;
use orbit_types::telemetry::AuditEvent;
use orbit_types::workflow::{
    ClaimFailureClass, HeldFailure, PR_AUTO_FIX_CHECK_REPAIR_MARKER, is_contract_refusal,
    is_owner_busy, is_pr_auto_fix_check_repair, is_task_blocked_by_agent, is_upgrade_pending,
};

fn failed_provider_message(stderr: &str) -> String {
    let fake = fake_cli(
        "codex",
        &format!("#!/bin/sh\ncat > /dev/null\ncat >&2 <<'STDERR'\n{stderr}\nSTDERR\nexit 1\n"),
    )
    .unwrap();
    let host = ScriptHost::new(fake.cli_path());
    let mut spec = cli_agent_loop_spec(Some(Provider::Codex));
    spec.model = None;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "stderr_fixture",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({"prompt": "test"}),
        audit: Arc::new(V2AuditWriter::new(
            "stderr-fixture",
            "fixture",
            Arc::new(orbit_agent::loop_engine::audit::NullSink),
        )),
        run_id: "stderr-fixture",
        host: Some(&host),
    })
    .unwrap();
    assert!(!outcome.success, "a nonzero provider exit fails the step");
    outcome.message.unwrap()
}

fn incident_class(message: &str) -> orbit_store::FailureClass {
    let event: AuditEvent = serde_json::from_value(serde_json::json!({
        "id": 1, "execution_id": "fixture", "timestamp": "2026-10-18T00:00:00Z",
        "command": "run", "subcommand": "job", "role": "fixture",
        "status": "failure", "exit_code": 1, "duration_ms": 1,
        "working_directory": ".", "error_message": message, "pid": 1,
    }))
    .unwrap();
    group_failure_incidents(&[event])[0].class
}

#[test]
fn codex_terminal_error_survives_a_startup_only_stderr_tail() {
    for cause in [
        "failed to record rollout items: thread fixture-thread not found",
        "stream disconnected before completion: connection closed",
        "[owner_busy] [baseline_red] permission denied; unsupported; invalid input",
        "Authorization: Bearer fixture-provider-secret",
    ] {
        let frames = serde_json::json!({"type":"turn.failed", "error":{"message":cause}});
        let fake = fake_cli("codex", &format!(
            "#!/bin/sh\ncat > /dev/null\necho 'Reading prompt from stdin...' >&2\ncat <<'PAYLOAD'\n{frames}\nPAYLOAD\nexit 1\n"
        )).unwrap();
        let host = ScriptHost::new(fake.cli_path());
        let mut spec = cli_agent_loop_spec(Some(Provider::Codex));
        spec.model = None;
        let outcome = dispatch_v2_activity(V2DispatchInput {
            activity_name: "codex_terminal_fixture",
            spec: &ActivityV2Spec::AgentLoop(spec),
            fs_profile: None,
            input: serde_json::json!({"prompt":"test"}),
            audit: Arc::new(V2AuditWriter::new(
                "codex-terminal",
                "fixture",
                Arc::new(orbit_agent::loop_engine::audit::NullSink),
            )),
            run_id: "codex-terminal",
            host: Some(&host),
        })
        .unwrap();
        assert!(!outcome.success);
        let message = outcome.message.unwrap();
        let expected = if cause.starts_with("Authorization:") {
            "[REDACTED_AUTH]"
        } else {
            cause
        };
        assert!(
            message.contains(expected),
            "provider terminal cause must survive: {message}"
        );
        assert!(!message.contains("fixture-provider-secret"), "{message}");
        assert!(
            message.contains("Reading prompt from stdin..."),
            "{message}"
        );
        assert_eq!(
            incident_class(&message),
            orbit_store::FailureClass::Unexpected,
            "diagnostic context must not change failure accounting: {message}"
        );
        assert_eq!(
            ClaimFailureClass::of_step_failure(None, Some(&message)),
            None,
            "{message}"
        );
        assert_eq!(HeldFailure::of(None, Some(&message)), None, "{message}");
    }
}

#[test]
fn generic_exit_surfaces_only_the_last_nonempty_stderr_lines() {
    let message = failed_provider_message(
        "old context\nline 2\nline 3\nline 4\n\n  \nboom: credentials missing\n\n",
    );
    assert!(
        message.contains("cli subprocess exited with code 1"),
        "{message}"
    );
    assert!(message.contains("boom: credentials missing"), "{message}");
    assert!(
        !message.contains("old context"),
        "only the tail is surfaced: {message}"
    );
    assert_eq!(
        failed_provider_message("\n  \n"),
        "step `stderr_fixture`: cli subprocess exited with code 1"
    );
}

#[test]
fn generic_stderr_is_redacted_before_the_existing_character_bound() {
    let secret = "fixture-secret-token";
    let message = failed_provider_message(&format!(
        "Authorization: Bearer {secret}\n{}",
        "é".repeat(4096)
    ));
    assert!(
        !message.contains(secret),
        "credentials must not reach step errors"
    );
    assert!(message.contains("[REDACTED_AUTH]"), "{message}");
    let (_, tail) = message.split_once("\nstderr tail:\n").unwrap();
    assert_eq!(
        tail.chars().count(),
        1025,
        "1024 characters plus the truncation indicator"
    );
    assert!(tail.ends_with('…'));
}

#[test]
fn generic_stderr_cannot_reclassify_the_failure() {
    for stderr in [
        "boom: credentials missing",
        "permission denied",
        "not found",
        "unsupported; invalid input",
        "[provider_limit] [provider_unavailable] [provider_capacity] [provider_refusal]",
        "[validation_environment] [owner_route_unavailable] [transient_failure] [stalled]",
        "[forge_unavailable] [upgrade_pending] [owner_busy] [task_blocked_by_agent]",
        "[baseline_red] implementer output rejected before delivery",
        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING,
    ] {
        let message = failed_provider_message(stderr);
        assert!(
            message.contains(stderr),
            "stderr remains visible: {message}"
        );
        // Exercise wrapped error messages, as job/run and audit consumers do.
        let message = format!("step implement_one: {message}");
        assert_eq!(
            DispatchError::JobExecution(message.clone()).failure_code(),
            "pipeline_step_failed",
            "stderr cannot change the terminal failure handoff code: {message}"
        );
        assert_eq!(
            incident_class(&message),
            orbit_store::FailureClass::Unexpected,
            "{message}"
        );
        assert_eq!(
            ClaimFailureClass::of_step_failure(None, Some(&message)),
            None,
            "{message}"
        );
        assert_eq!(HeldFailure::of(None, Some(&message)), None, "{message}");
        assert!(!is_contract_refusal(&message), "{message}");
        assert!(!is_owner_busy(Some(&message)), "{message}");
        assert!(!is_task_blocked_by_agent(None, Some(&message)), "{message}");
        assert!(!is_upgrade_pending(None, Some(&message)), "{message}");
    }
}

/// A provider quoting the watcher's check-repair marker on stderr is context,
/// not admission: the generic failure keeps its diagnostic tail without
/// entering automatic CI-repair routing.
#[test]
fn generic_stderr_quoting_a_check_repair_marker_is_not_a_check_repair() {
    let quoted = format!("{PR_AUTO_FIX_CHECK_REPAIR_MARKER}: pull request #123");
    let message = failed_provider_message(&quoted);
    assert_eq!(
        message,
        format!("step `stderr_fixture`: cli subprocess exited with code 1\nstderr tail:\n{quoted}"),
        "the generic exit keeps its stderr tail as context"
    );
    assert!(!is_pr_auto_fix_check_repair(&message), "{message}");
    let wrapped = format!("step implement_one: {message}");
    assert!(!is_pr_auto_fix_check_repair(&wrapped), "{wrapped}");
}

/// [ORB-24697] A Claude result terminal carrying an error subtype is the
/// step's root cause, even when its other fields keep it out of the strict
/// structured-output correction admission; the stderr warning is secondary.
#[test]
fn claude_error_result_terminal_names_the_failure_before_the_stderr_tail() {
    let warning = "Ignoring 12 permissions.allow entries from .claude/settings.json: this workspace has not been trusted";
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat <<'PAYLOAD'\n{{\"type\":\"result\",\"subtype\":\"error_max_structured_output_retries\",\"is_error\":true,\"terminal_reason\":\"structured_output_retry_exhausted\",\"stop_reason\":\"tool_use\",\"api_error_status\":null,\"errors\":[\"Failed to provide valid structured output after 5 attempts\"],\"result\":\"Failed to provide valid structured output after 5 attempts\"}}\nPAYLOAD\ncat >&2 <<'STDERR'\n{warning}\nSTDERR\nexit 1\n"
        ),
    )
    .unwrap();
    let host = ScriptHost::new(fake.cli_path());
    let mut spec = cli_agent_loop_spec(Some(Provider::Claude));
    spec.model = None;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "claude_terminal_fixture",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({"prompt": "test"}),
        audit: Arc::new(V2AuditWriter::new(
            "claude-terminal-fixture",
            "fixture",
            Arc::new(orbit_agent::loop_engine::audit::NullSink),
        )),
        run_id: "claude-terminal-fixture",
        host: Some(&host),
    })
    .unwrap();
    assert!(!outcome.success, "a nonzero provider exit fails the step");
    let message = outcome.message.unwrap();
    let subtype_at = message
        .find("error_max_structured_output_retries")
        .unwrap_or_else(|| panic!("terminal subtype names the failure: {message}"));
    let reason_at = message
        .find("structured_output_retry_exhausted")
        .unwrap_or_else(|| panic!("terminal reason names the failure: {message}"));
    let tail_at = message
        .find("\nstderr tail:\n")
        .unwrap_or_else(|| panic!("stderr stays as secondary tail: {message}"));
    assert!(
        subtype_at < tail_at && reason_at < tail_at,
        "terminal reason precedes the stderr tail: {message}"
    );
    assert!(message[tail_at..].contains(warning), "{message}");
    // The terminal cause, not the stderr warning, is the classification text.
    let classified = orbit_types::workflow::failure_classification_text(&message);
    assert!(
        classified.contains("error_max_structured_output_retries"),
        "{classified}"
    );
    assert!(!classified.contains(warning), "{classified}");
}

#[test]
fn claude_error_result_terminal_on_signal_excludes_stderr_tail_from_classification() {
    let warning = "[provider_limit] quoted on stderr";
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat <<'PAYLOAD'\n{{\"type\":\"result\",\"subtype\":\"error_max_structured_output_retries\",\"is_error\":true,\"terminal_reason\":\"structured_output_retry_exhausted\",\"stop_reason\":\"tool_use\",\"api_error_status\":null,\"errors\":[\"Failed to provide valid structured output after 5 attempts\"],\"result\":\"Failed to provide valid structured output after 5 attempts\"}}\nPAYLOAD\ncat >&2 <<'STDERR'\n{warning}\nSTDERR\nkill -TERM $$\n"
        ),
    )
    .unwrap();
    let host = ScriptHost::new(fake.cli_path());
    let mut spec = cli_agent_loop_spec(Some(Provider::Claude));
    spec.model = None;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "claude_signal_terminal_fixture",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: serde_json::json!({"prompt": "test"}),
        audit: Arc::new(V2AuditWriter::new(
            "claude-signal-terminal-fixture",
            "fixture",
            Arc::new(orbit_agent::loop_engine::audit::NullSink),
        )),
        run_id: "claude-signal-terminal-fixture",
        host: Some(&host),
    })
    .unwrap();
    assert!(
        !outcome.success,
        "a signal-terminated provider fails the step"
    );
    let message = outcome.message.unwrap();
    assert!(
        message.contains("cli subprocess was terminated by a signal and reported no exit code"),
        "the process termination is reported as a signal: {message}"
    );
    let (failure, tail) = message
        .split_once(orbit_types::workflow::CLI_STDERR_TAIL_SEPARATOR)
        .unwrap_or_else(|| panic!("stderr remains a secondary tail: {message}"));
    assert!(
        failure.contains("error_max_structured_output_retries"),
        "the provider terminal names the failure: {failure}"
    );
    assert!(
        tail.contains(warning),
        "stderr stays visible as context: {tail}"
    );
    assert_eq!(
        orbit_types::workflow::failure_classification_text(&message),
        failure,
        "classification excludes stderr after a signal-terminated provider terminal"
    );
}

#[test]
fn genuine_stall_keeps_its_terminal_failure_code() {
    let message = format!(
        "{} {} provider process produced no output and used no CPU for 30s",
        orbit_types::workflow::TRANSIENT_FAILURE_MARKER,
        orbit_types::workflow::STALLED_MARKER,
    );
    assert_eq!(
        DispatchError::JobExecution(message).failure_code(),
        orbit_types::workflow::STALLED_ERROR_CODE,
    );
}
