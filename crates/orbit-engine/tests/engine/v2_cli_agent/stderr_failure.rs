//! Generic subprocess failures retain bounded operator evidence without
//! letting untrusted stderr change incident or workflow classification.

use super::*;
use orbit_store::contracts::group_failure_incidents;
use orbit_types::telemetry::AuditEvent;
use orbit_types::workflow::{
    ClaimFailureClass, HeldFailure, is_contract_refusal, is_owner_busy, is_task_blocked_by_agent,
    is_upgrade_pending,
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
