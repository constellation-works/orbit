use std::time::Duration;

use orbit_common::process::build_budget::BuildBudgetWaits;
use orbit_common::security::redaction::PatternRedactor;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use orbit_types::workflow::{
    ClaimFailureClass, STALLED_ERROR_CODE, STALLED_MARKER, TRANSIENT_FAILURE_MARKER,
};
use serde_json::json;

use super::super::super::supervisor::CapturedOutput;
use super::super::completion::{ProviderExit, print_budget_exhausted, project_completion};
use crate::DispatchError;
use crate::context::NullRuntimeHost;

// Combinatorial budget/exit/envelope logic: exact hour-long boundaries cannot
// be exercised economically by a real provider process. [ORB-15206]
#[test]
fn only_a_clean_exit_without_an_envelope_at_the_injected_budget_is_a_timeout() {
    let mut argv = Vec::new();
    let budget = orbit_agent::apply_antigravity_print_timeout(
        "antigravity",
        &mut argv,
        Duration::from_secs(3600),
    );
    assert_eq!(budget, Some(Duration::from_secs(3570)));
    for (exit_success, missing, budget, elapsed_ms, expected) in [
        (true, true, budget, 3_569_999, false),
        (true, true, budget, 3_570_000, true),
        (true, true, budget, 3_570_001, true),
        (true, false, budget, 3_570_000, false),
        (false, true, budget, 3_570_000, false),
        (true, true, None, 3_570_000, false),
    ] {
        assert_eq!(
            print_budget_exhausted(
                exit_success,
                missing,
                budget,
                Duration::from_millis(elapsed_ms),
            ),
            expected,
            "ORB-15206: exit_success={exit_success}, missing={missing}, budget={budget:?}, elapsed={elapsed_ms}ms",
        );
    }
}

struct IdleHost;

impl NullRuntimeHost for IdleHost {}

fn project(stalled: bool, timed_out: bool) -> crate::DispatchOutcome {
    project_stdout("fixture", b"", false, stalled, timed_out)
}

fn project_stdout(
    provider: &str,
    stdout: &[u8],
    exit_success: bool,
    stalled: bool,
    timed_out: bool,
) -> crate::DispatchOutcome {
    let spec: AgentLoopSpec = serde_json::from_value(json!({})).expect("empty agent spec");
    let host = IdleHost;
    let input = json!({});
    let redaction = PatternRedactor::http_default();
    project_completion(ProviderExit {
        host: &host,
        spec: &spec,
        input: &input,
        run_id: "run",
        provider: provider.to_string(),
        model: None,
        task_ids: &[],
        worktree_boundary: None,
        sandbox: None,
        subprocess_cwd: None,
        redaction: &redaction,
        timeout_seconds: 300,
        argv_redacted: Vec::new(),
        stdin_blob_ref: "stdin".to_string(),
        stdout_blob_ref: "stdout".to_string(),
        stderr_blob_ref: "stderr".to_string(),
        stdout: CapturedOutput::from_bytes(stdout),
        stderr: CapturedOutput::empty(),
        exit_code: exit_success.then_some(0),
        duration: Duration::from_millis(700),
        timed_out,
        stalled,
        stall_window: Duration::from_secs(30),
        print_timeout: None,
        usage_context: Default::default(),
        build_budget_waits: BuildBudgetWaits::default(),
    })
    .expect("project a provider exit")
}

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
    for noise in ["not json at all", r#"{"type":"assistant","text":"\ud83d"}"#] {
        for (frame, expected) in [
            (limit, "usage limit"),
            (auth, "authentication failure (HTTP 401)"),
            (refusal, "declined"),
        ] {
            let stdout = format!("{noise}\n{frame}\n");
            let outcome = project_stdout("claude", stdout.as_bytes(), true, false, false);
            assert!(!outcome.success, "{noise} / {frame}");
            let message = outcome.message.expect("provider failure message");
            assert!(message.contains(expected), "{noise} / {frame}: {message}");
        }
    }
}
