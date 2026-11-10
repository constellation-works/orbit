use super::super::super::super::response_diagnostics::declared_failure_code;
use orbit_types::workflow::{
    ClaimFailureClass, HeldFailure, OWNER_SIGNOFF_REQUIRED_ERROR_CODE, STALLED_ERROR_CODE,
    STALLED_MARKER, StepFailureCode, TRANSIENT_FAILURE_MARKER, is_owner_route_unavailable,
};
use serde_json::json;

use super::projection::{project, project_stdout};
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
