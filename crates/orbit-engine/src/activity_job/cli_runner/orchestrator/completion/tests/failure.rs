use super::super::super::super::response_diagnostics::declared_failure_code;
use orbit_types::workflow::{
    ClaimFailureClass, HeldFailure, OWNER_SIGNOFF_REQUIRED_ERROR_CODE, ProviderFailureClass,
    ProviderLimitFailure, STALLED_ERROR_CODE, STALLED_MARKER, StepFailureCode,
    TRANSIENT_FAILURE_MARKER, failed_provider, is_owner_route_unavailable, is_provider_unavailable,
};
use serde_json::json;

use super::projection::{project, project_provider_output, project_stdout};
use crate::DispatchError;

#[test]
fn a_stalled_provider_fails_transient_without_a_wall_clock_timeout() {
    for (stalled, timed_out, expected_marker, absent_marker) in [
        (true, false, STALLED_MARKER, "wall-clock timeout"),
        (false, true, "wall-clock timeout", STALLED_MARKER),
    ] {
        let outcome = project(stalled, timed_out);
        assert!(!outcome.success);
        assert_eq!(outcome.output["stalled"], json!(stalled));
        assert_eq!(outcome.output["timed_out"], json!(timed_out));
        let message = outcome.message.expect("provider failure message");
        assert!(message.contains(expected_marker), "{message}");
        assert!(!message.contains(absent_marker), "{message}");

        if stalled {
            assert!(message.contains(TRANSIENT_FAILURE_MARKER), "{message}");
            assert!(message.contains("30s"), "{message}");
            assert_eq!(
                ClaimFailureClass::of_step_failure(Some(STALLED_ERROR_CODE), Some(&message)),
                Some(ClaimFailureClass::Transient)
            );
            let error = DispatchError::JobExecution(message);
            assert_eq!(error.failure_code(), STALLED_ERROR_CODE);
            assert!(!error.is_non_retryable());
        }
    }
}

// A slice stream reader stops at its first parse error, so an undecodable
// stdout line used to hide the terminal failure frame behind it.
#[test]
fn a_terminal_failure_frame_is_classified_after_an_undecodable_stdout_line() {
    let limit = r#"{"type":"result","is_error":true,"result":"You've hit your usage limit"}"#;
    let auth =
        r#"{"type":"result","is_error":true,"api_error_status":401,"result":"Invalid API key"}"#;
    let refusal =
        r#"{"type":"result","is_error":false,"stop_reason":"refusal","result":"declined"}"#;
    let mut failures = Vec::new();
    for noise in [
        "",
        "not json at all",
        r#"{"type":"assistant","text":"\ud83d"}"#,
        "{",
        r#"{"partial":"#,
    ] {
        for (frame, expected, marker) in [
            (limit, "usage limit", "[provider_limit]"),
            (
                auth,
                "authentication failure (HTTP 401)",
                "[provider_unavailable]",
            ),
            (refusal, "declined", "[provider_refusal]"),
        ] {
            let multiline = serde_json::to_string_pretty(
                &serde_json::from_str::<serde_json::Value>(frame).expect("terminal frame"),
            )
            .expect("multiline terminal frame");
            for frame in [frame, multiline.as_str()] {
                let stdout =
                    format!("{{\"type\":\"assistant\",\"text\":\"working\"}}\n{noise}\n{frame}\n");
                let outcome = project_stdout("claude", stdout.as_bytes(), true, false, false);
                let message = outcome.message.unwrap_or_default();
                if outcome.success || !message.contains(expected) || !message.contains(marker) {
                    failures.push(format!("{noise} / {frame}: {message}"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// [ORB-24694] Codex's quota/billing turn failure is provider-unavailable,
/// like a Claude authentication failure, not a bare exit code. Other Codex
/// exits, and quota text the agent itself wrote, keep their classification.
#[test]
fn a_codex_quota_turn_failure_is_provider_unavailable() {
    const QUOTA: &str = "Quota exceeded. Check your plan and billing details.";
    let frames = format!(
        "{{\"type\":\"error\",\"message\":\"{QUOTA}\"}}\n\
         {{\"type\":\"turn.failed\",\"error\":{{\"message\":\"{QUOTA}\"}}}}"
    );
    for exit_success in [false, true] {
        let outcome = project_stdout("codex", frames.as_bytes(), exit_success, false, false);
        assert!(!outcome.success, "quota turn must fail: {outcome:?}");
        let message = outcome.message.as_deref().unwrap_or_default();
        assert!(is_provider_unavailable(None, Some(message)), "{message}");
        assert!(message.contains("[provider_unavailable]"), "{message}");
        assert_eq!(failed_provider(message), Some("codex"), "{message}");
        assert!(message.contains(QUOTA), "{message}");
        assert!(
            !message.contains("stderr tail"),
            "not a bare exit/stderr root cause: {message}"
        );
        assert_eq!(
            ClaimFailureClass::of_step_failure(None, Some(message)),
            Some(ClaimFailureClass::Provider),
            "{message}"
        );
    }

    // A task-level failure after work, and agent text quoting the quota
    // phrase, are not the provider reporting its own quota.
    for stdout in [
        r#"{"type":"item.completed","item":{"type":"command_execution","command":"cargo test","aggregated_output":"boom","exit_code":1,"status":"failed"}}"#
            .to_string(),
        format!(
            r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"{QUOTA}"}}}}}}"#
        ),
    ] {
        let outcome = project_stdout("codex", stdout.as_bytes(), false, false, false);
        assert!(!outcome.success, "{outcome:?}");
        let message = outcome.message.as_deref().unwrap_or_default();
        assert!(
            !is_provider_unavailable(None, Some(message)),
            "ordinary Codex exit keeps its classification: {message}"
        );
    }
    // Billing quota, timed usage limits and host disk quota must retain distinct
    // completion classifications, even when a limit also says quota exceeded.
    const GEMINI_LIMIT: &str = "You exceeded your current quota, please check your plan and billing details. Quota exceeded for metric: generativelanguage.googleapis.com/generate_content_free_tier_requests, limit: 10. Please retry in 34s.";
    const BILLING: &str = "Quota exceeded. Check your plan and billing details.";
    const DISK: &str = "Disk quota exceeded (os error 122)";
    let overlapping_limit = format!("{BILLING}\nUsage limit reached. Please retry in 34s.");
    let mut failures = Vec::new();
    for (provider, text, expected, frame) in [
        (
            "gemini",
            GEMINI_LIMIT,
            "limit",
            json!({"type":"error", "error":{"message":GEMINI_LIMIT}}).to_string(),
        ),
        (
            "claude",
            overlapping_limit.as_str(),
            "limit",
            format!(
                "{}\n{}",
                json!({"type":"result", "is_error":true, "result":BILLING}),
                json!({"type":"result", "is_error":true, "result":overlapping_limit}),
            ),
        ),
        (
            "codex",
            overlapping_limit.as_str(),
            "limit",
            json!({"type":"turn.failed", "error":{"message":overlapping_limit}}).to_string(),
        ),
        (
            "codex",
            BILLING,
            "billing",
            json!({"type":"turn.failed", "error":{"message":BILLING}}).to_string(),
        ),
        (
            "codex",
            DISK,
            "host",
            json!({"type":"turn.failed", "error":{"message":DISK}}).to_string(),
        ),
    ] {
        let frame = frame.as_bytes();
        for (stdout, stderr, exit_success) in [
            (frame, b"".as_slice(), false),
            (b"".as_slice(), text.as_bytes(), false),
        ]
        .into_iter()
        .chain((provider == "claude" || expected == "billing").then_some((
            frame,
            b"".as_slice(),
            true,
        ))) {
            let before = chrono::Utc::now();
            let outcome =
                project_provider_output(provider, stdout, stderr, exit_success, false, false);
            let after = chrono::Utc::now();
            let message = outcome.message.as_deref().unwrap_or_default();
            let limit = ProviderLimitFailure::from_text(message);
            let classified_correctly = match expected {
                "limit" => {
                    limit
                        .and_then(|limit| limit.resets_at)
                        .is_some_and(|reset| {
                            reset >= before + chrono::Duration::seconds(33)
                                && reset <= after + chrono::Duration::seconds(34)
                        })
                        && ProviderFailureClass::of(None, Some(message))
                            == Some(ProviderFailureClass::Limit)
                }
                "billing" => is_provider_unavailable(None, Some(message)) && limit.is_none(),
                "host" => !is_provider_unavailable(None, Some(message)) && limit.is_none(),
                _ => unreachable!("known fixture classification"),
            };
            if outcome.success || !classified_correctly {
                failures.push(format!(
                    "{provider} {expected} stdout={} exit_success={exit_success}: {message}",
                    !stdout.is_empty()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// [ORB-25393] A non-reserved declared code names a blocker; reserved,
/// provider/transient and malformed declarations do not.
#[test]
fn a_declared_failure_code_names_a_blocker_only_when_it_is_not_reserved_or_malformed() {
    use orbit_types::workflow::{
        AgentBlockerClass, agent_blocker_from_declared_failure, is_reserved_declared_failure_code,
    };

    let blocker =
        agent_blocker_from_declared_failure("validation_blocker", "needs a live pilot check")
            .expect("task-level blocker");
    assert_eq!(blocker.kind, "validation_blocker");
    assert_eq!(blocker.evidence, "needs a live pilot check");
    assert_eq!(blocker.class(), AgentBlockerClass::Task);
    assert!(
        agent_blocker_from_declared_failure("task_blocked", "already done elsewhere").is_some()
    );
    assert!(
        agent_blocker_from_declared_failure("contract_contradiction", "criteria conflict")
            .is_some()
    );

    for reserved in [
        "implementation_incomplete",
        "owner_route_unavailable",
        "owner_signoff_required",
        "footprint_unavailable",
        "provider_unavailable",
        "provider_limit",
        "provider_capacity",
        "provider_refusal",
        "validation_environment",
        "validation_route_unavailable",
        "transient_failure",
        "stalled",
        "task_blocked_by_agent",
        "upgrade_pending",
    ] {
        assert!(
            is_reserved_declared_failure_code(reserved),
            "{reserved} is reserved"
        );
        assert_eq!(
            agent_blocker_from_declared_failure(reserved, "evidence"),
            None,
            "{reserved} keeps its existing outcome"
        );
    }

    for (code, evidence) in [
        ("", "evidence"),
        ("9bad", "evidence"),
        ("has space", "evidence"),
        ("validation_blocker", ""),
        ("validation_blocker", "  "),
    ] {
        assert_eq!(
            agent_blocker_from_declared_failure(code, evidence),
            None,
            "malformed declaration {code:?}/{evidence:?} is not a blocker"
        );
    }

    // A malformed envelope (no `error.message`) carries no typed code at
    // ingestion, so the step boundary has nothing to normalize.
    let malformed = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"validation_blocker"}}"#;
    let outcome = project_stdout("fixture", malformed.as_bytes(), true, false, false);
    assert!(!outcome.success);
    assert_eq!(outcome.failure_code, None, "{outcome:?}");
}

/// [ORB-23956] An implement step that declares `owner_signoff_required` is
/// waiting for recorded owner approval. That stop is a hold, not a failed run.
#[test]
fn an_owner_signoff_required_envelope_holds_instead_of_failing_the_run() {
    let declared = "The validated candidate awaits recorded owner approval.";
    let envelope = format!(
        r#"{{"schemaVersion":1,"status":"failed","result":{{}},"error":{{"code":"{OWNER_SIGNOFF_REQUIRED_ERROR_CODE}","message":"{declared}"}}}}"#
    );
    let parsed = orbit_agent::ParsedStdout::parse(&envelope);
    let failure = parsed
        .peek_declared_response_failure()
        .expect("declared failure");
    let code = declared_failure_code(Some(&failure)).expect("typed code from the envelope");
    let parsed_code = StepFailureCode::parse(OWNER_SIGNOFF_REQUIRED_ERROR_CODE);

    assert_eq!(code, parsed_code);
    assert_eq!(code, StepFailureCode::OwnerSignoffRequired);
    assert!(
        !matches!(parsed_code, StepFailureCode::Other(_)),
        "owner_signoff_required is a typed step code"
    );
    assert!(!parsed_code.is_owner_route_unavailable());
    assert!(!is_owner_route_unavailable(
        Some(&parsed_code),
        Some(declared)
    ));

    let outcome = project_stdout("fixture", envelope.as_bytes(), true, false, false);
    assert!(!outcome.success, "the implement step stopped");
    assert_eq!(outcome.failure_code.as_ref(), Some(&code));
    let message = outcome.message.expect("declared diagnostic");
    assert!(message.contains(declared), "{message}");
    assert!(
        message.contains("error.code=owner_signoff_required;"),
        "{message}"
    );

    // `HeldFailure::of_local` is what an owner-local run, including
    // task_pr_pipeline, uses to store Held rather than Failed. The declared
    // message is the hold reason.
    let held = HeldFailure::of_local(Some(code.as_str()), Some(&message));
    assert_eq!(held, Some(HeldFailure::OwnerSignoffRequired));
    let held = held.expect("hold");
    assert_eq!(held.error_code(), OWNER_SIGNOFF_REQUIRED_ERROR_CODE);
    assert_eq!(held.diagnostic(&message), message);

    let job_error = DispatchError::JobExecution(message);
    assert_eq!(job_error.failure_code(), OWNER_SIGNOFF_REQUIRED_ERROR_CODE);
}
