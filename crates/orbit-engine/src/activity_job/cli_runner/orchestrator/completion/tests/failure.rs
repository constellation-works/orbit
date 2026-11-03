use orbit_types::workflow::{
    ClaimFailureClass, STALLED_ERROR_CODE, STALLED_MARKER, TRANSIENT_FAILURE_MARKER,
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
