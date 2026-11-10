//! [ORB-23898] A provider stream that outgrows the bounded capture keeps only
//! its head and final complete lines. When that tail ends on an assistant
//! message carrying the Orbit envelope and no terminal `result` frame, the
//! answer projection reads nothing, yet the final message holds a valid
//! envelope. The envelope is accepted through the same validation gates, the
//! capture bound is unchanged, and a truncated capture with no envelope anywhere
//! still fails with the missing-envelope protocol violation.

use serde_json::json;

use super::*;

const CAPTURE_LIMIT_ENV: &str = "ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES";
/// Small enough that the padding below outgrows it several times over.
const CAPTURE_LIMIT: usize = 4_096;
const VALID: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"summary":"the answer"},"error":null}"#;
const CONTRADICTORY: &str = r#"{"schemaVersion":1,"status":"success","result":{"summary":"the answer"},"error":{"code":"none","message":"nothing went wrong"}}"#;

/// A Claude stream that outgrows the capture and ends on one assistant message
/// whose text is `answer`, with no terminal `result` frame after it.
fn stream_ending_on_message(answer: &str) -> String {
    let padding = json!({"type":"stream_event","event":{"type":"content_block_delta",
        "delta":{"type":"text_delta","text":"padding ".repeat(16)}}});
    let assistant = json!({"type":"assistant","message":{"role":"assistant",
        "content":[{"type":"text","text":answer}]}});
    let mut frames = vec![padding; 128];
    frames.push(assistant);
    frames
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dispatches one agent-loop activity whose scripted `claude` prints `stream`
/// through a capture bounded at [`CAPTURE_LIMIT`] bytes.
fn dispatch_truncated(stream: &str, run_id: &str) -> orbit_engine::DispatchOutcome {
    let data = tempfile::tempdir().unwrap();
    let stream_path = data.path().join("stream.jsonl");
    fs::write(&stream_path, format!("{stream}\n")).unwrap();
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\ncat '{}'\n",
            stream_path.display()
        ),
    )
    .unwrap();

    // SAFETY: nextest runs each test in its own process; the variable is set
    // here and removed below, once dispatch has returned.
    unsafe { std::env::set_var(CAPTURE_LIMIT_ENV, CAPTURE_LIMIT.to_string()) };
    let tmp_audit = tempfile::tempdir().unwrap();
    let (writer, _store) = build_writer(tmp_audit.path(), run_id).unwrap();
    let host = ScriptHost::new(fake.cli_path());
    let mut spec = cli_agent_loop_spec(None);
    spec.require_response_envelope = true;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "cli_truncated_envelope",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "prompt": "implement" }),
        audit: writer,
        run_id,
        host: Some(&host),
    })
    .unwrap();
    // SAFETY: same single-test process as the set_var above; dispatch has returned.
    unsafe { std::env::remove_var(CAPTURE_LIMIT_ENV) };

    assert_eq!(
        outcome.output["stdout_capture_truncated"], true,
        "the fixture must outgrow the capture"
    );
    outcome
}

#[cfg(unix)]
#[test]
fn truncated_capture_accepts_a_valid_final_message_envelope() {
    crate::git_fixture::isolated(
        module_path!(),
        "truncated_capture_accepts_a_valid_final_message_envelope",
        || {
            let outcome =
                dispatch_truncated(&stream_ending_on_message(VALID), "truncated-envelope-valid");
            assert!(outcome.success, "{:?}", outcome.message);
            assert_eq!(outcome.output["completion_envelope_satisfied"], true);
            assert_eq!(outcome.output["response_envelope_valid"], true);
            assert_eq!(outcome.output["response_envelope_status"], "success");
        },
    );
}

#[cfg(unix)]
#[test]
fn truncated_capture_without_a_final_message_envelope_still_fails() {
    crate::git_fixture::isolated(
        module_path!(),
        "truncated_capture_without_a_final_message_envelope_still_fails",
        || {
            let outcome = dispatch_truncated(
                &stream_ending_on_message("I finished the work."),
                "truncated-envelope-absent",
            );
            assert!(!outcome.success);
            assert_eq!(outcome.output["completion_envelope_satisfied"], false);
            let error = outcome.output["completion_envelope_error"]
                .as_str()
                .expect("missing-envelope diagnostic");
            assert!(
                error.contains("stdout does not contain an Orbit response envelope"),
                "expected the missing-envelope protocol violation, got {error}"
            );
        },
    );
}

#[cfg(unix)]
#[test]
fn truncated_capture_recovered_envelope_keeps_status_validation() {
    crate::git_fixture::isolated(
        module_path!(),
        "truncated_capture_recovered_envelope_keeps_status_validation",
        || {
            let outcome = dispatch_truncated(
                &stream_ending_on_message(CONTRADICTORY),
                "truncated-envelope-contradictory",
            );
            assert!(!outcome.success);
            let error = outcome.output["response_envelope_error"]
                .as_str()
                .expect("envelope validation diagnostic");
            assert!(
                error.contains("success status must not carry error object"),
                "expected the status/error consistency violation, got {error}"
            );
        },
    );
}
