//! [ORB-14696] After every Codex or Claude run, the host records the
//! provider's own reading of each usage window. Run through
//! `dispatch_v2_activity` against fake provider CLIs that print recorded
//! output.
//!
//! Fixtures under `fixtures/provider_usage/`:
//!
//! - `codex_exec_stdout.jsonl`: a real `codex exec --json` run (codex-cli
//!   0.161.0), and `codex_rollout.jsonl` its session rollout, kept to the
//!   `session_meta` and `event_msg` lines with the cwd and credit balance
//!   scrubbed and `primary.used_percent` set to the 21 seen in the incident.
//! - `claude_stream.jsonl`: a real `claude -p --json-schema … --output-format
//!   stream-json --verbose` run (Claude Code 2.1.293), with the init message
//!   cut to its identifying fields, the thinking signature redacted, the
//!   envelope set to Orbit's, and the window utilizations set to 0.91 and
//!   0.42.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Local, Utc};
use orbit_engine::{DispatchOutcome, V2DispatchInput, dispatch_v2_activity};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource, TokenUsage};
use orbit_types::workflow::activity_job::{ActivityV2Spec, Provider};
use serde_json::{Value, json};

use super::provider_capacity::{CapacityHost, FakeProvider, agent_spec, writer};

const CODEX_STDOUT: &str = include_str!("fixtures/provider_usage/codex_exec_stdout.jsonl");
const CODEX_ROLLOUT: &str = include_str!("fixtures/provider_usage/codex_rollout.jsonl");
const CODEX_THREAD: &str = "01a11e3d-4d1d-7102-a520-fcba3210177d";
const CLAUDE_STREAM: &str = include_str!("fixtures/provider_usage/claude_stream.jsonl");

/// Dispatch a fake `binary` that prints `stdout`, with `agent_env` in the
/// child's environment, and return the outcome and the limits recorded.
fn dispatch(
    binary: &str,
    provider: Provider,
    stdout: &str,
    agent_env: &[(&str, &Path)],
) -> (DispatchOutcome, Vec<ProviderLimitObservation>) {
    let fake = FakeProvider::new(binary, stdout, "", 0, "");
    let audit = tempfile::tempdir().unwrap();
    let mut host = CapacityHost::new(&fake.path, audit.path());
    host.agent_env = agent_env
        .iter()
        .map(|(key, value)| (key.to_string(), value.display().to_string()))
        .collect();
    let mut spec = agent_spec(provider);
    spec.require_response_envelope = true;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "usage_window_fixture",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "prompt": "implement", "crew": "fixture-crew" }),
        audit: writer(audit.path(), "usage-run"),
        run_id: "usage-run",
        host: Some(&host),
    })
    .unwrap();
    let limits = host.limits.lock().unwrap().clone();
    (outcome, limits)
}

/// A `CODEX_HOME` holding `rollout` as the session rollout of `thread_id`,
/// under today's local date as Codex files it.
fn codex_home(thread_id: &str, rollout: Option<&str>) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let day = home
        .path()
        .join("sessions")
        .join(Local::now().format("%Y/%m/%d").to_string());
    fs::create_dir_all(&day).unwrap();
    if let Some(rollout) = rollout {
        fs::write(
            day.join(format!("rollout-2026-10-08T18-18-16-{thread_id}.jsonl")),
            rollout,
        )
        .unwrap();
    }
    home
}

/// The recorded rollout with its `token_count` carrying no rate limits.
fn rollout_without_rate_limits() -> String {
    CODEX_ROLLOUT
        .lines()
        .map(|line| {
            let mut frame: Value = serde_json::from_str(line).unwrap();
            if frame["payload"]["type"] == "token_count" {
                frame["payload"]["rate_limits"] = Value::Null;
            }
            frame.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_codex_run_records_the_window_its_own_rollout_reports() {
    let home = codex_home(CODEX_THREAD, Some(CODEX_ROLLOUT));
    let (outcome, limits) = dispatch(
        "codex",
        Provider::Codex,
        CODEX_STDOUT,
        &[("CODEX_HOME", home.path())],
    );
    // The recorded answer is `OK`, not an envelope: the step fails, and the
    // reading is recorded all the same.
    assert!(!outcome.success, "{:?}", outcome.message);
    let [reading] = limits.as_slice() else {
        panic!("one window, one observation: {limits:?}");
    };
    assert_eq!(reading.provider, "codex");
    assert_eq!(reading.source, ProviderLimitSource::Event);
    assert_eq!(reading.model, None, "codex windows are account-wide");
    assert_eq!(reading.window.as_deref(), Some("primary"));
    assert_eq!(reading.used_percent, Some(21.0));
    assert_eq!(reading.window_minutes, Some(10080));
    assert_eq!(
        reading.resets_at,
        DateTime::from_timestamp(1_791_948_575, 0),
        "the reset codex reported"
    );
    assert!(!reading.exhausted);
    assert!(reading.gating);
    assert_eq!(
        reading.observed_at,
        "2026-10-09T01:18:19.662Z".parse::<DateTime<Utc>>().unwrap(),
        "observed when codex wrote the reading"
    );
    assert_eq!(reading.run_id.as_deref(), Some("usage-run"));
    assert_eq!(reading.crew.as_deref(), Some("fixture-crew"));
    assert_eq!(reading.detail, "limit_id=codex plan_type=prolite");

    let other_thread = codex_home("01a11e3d-0000-7000-8000-000000000000", Some(CODEX_ROLLOUT));
    let missing = codex_home(CODEX_THREAD, None);
    let without_rate_limits = rollout_without_rate_limits();
    let null_limits = codex_home(CODEX_THREAD, Some(&without_rate_limits));
    for (label, home) in [
        ("another thread's rollout", &other_thread),
        ("no rollout", &missing),
        ("rate_limits null", &null_limits),
    ] {
        let (outcome, limits) = dispatch(
            "codex",
            Provider::Codex,
            CODEX_STDOUT,
            &[("CODEX_HOME", home.path())],
        );
        assert!(!outcome.success, "{label}: {:?}", outcome.message);
        assert!(limits.is_empty(), "{label} records nothing: {limits:?}");
    }
}

/// The values a run's outcome carries that the response and usage
/// projection decide.
fn projected(outcome: &DispatchOutcome) -> (Value, TokenUsage) {
    let output = &outcome.output;
    let response = json!({
        "success": outcome.success,
        "message": outcome.message,
        "response_envelope_valid": output["response_envelope_valid"],
        "response_envelope_status": output["response_envelope_status"],
        "response_result_fields": output["response_result_fields"],
        "completion_envelope_satisfied": output["completion_envelope_satisfied"],
        "final_message": output["final_message"],
    });
    let usage = outcome
        .invocation
        .as_ref()
        .expect("the invocation is traced")
        .trace
        .usage
        .clone();
    (response, usage)
}

#[test]
fn a_claude_stream_records_both_windows_and_projects_as_its_result_alone() {
    let (outcome, limits) = dispatch("claude", Provider::Claude, CLAUDE_STREAM, &[]);
    assert!(outcome.success, "{:?}", outcome.message);
    let argv = outcome.output["argv_redacted"].as_array().unwrap();
    let flags: Vec<_> = argv.iter().filter_map(Value::as_str).collect();
    assert!(
        flags
            .windows(3)
            .any(|flags| flags == ["--output-format", "stream-json", "--verbose"]),
        "the stream exposes rate_limit_event: {flags:?}"
    );

    let windows: Vec<_> = limits
        .iter()
        .map(|reading| {
            assert_eq!(reading.provider, "claude");
            assert_eq!(reading.source, ProviderLimitSource::Event);
            assert_eq!(reading.model, None);
            assert!(!reading.exhausted && reading.gating, "{reading:?}");
            assert_eq!(reading.run_id.as_deref(), Some("usage-run"));
            (
                reading.window.as_deref().unwrap(),
                reading.used_percent,
                reading.window_minutes,
                reading.resets_at.map(|at| at.timestamp()),
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("five_hour", Some(91.0), Some(300), Some(1_791_525_000)),
            ("seven_day", Some(42.0), Some(10080), Some(1_791_554_400)),
        ]
    );

    // The terminal `result` alone is what `--output-format json` printed.
    let result = CLAUDE_STREAM.lines().last().unwrap();
    let (single, single_limits) = dispatch("claude", Provider::Claude, result, &[]);
    assert!(single_limits.is_empty(), "the result frame has no windows");
    let (stream_response, stream_usage) = projected(&outcome);
    assert_eq!((stream_response, stream_usage.clone()), projected(&single));
    assert_eq!(
        stream_usage,
        TokenUsage {
            input: 2,
            cache_read: 0,
            cache_create: 0,
            cache_create_1h: 7875,
            output: 137,
        },
        "the result's totals, without the assistant message's own usage \
         (ORB-10906 / F2026-08-031 cache double-count)"
    );
}

/// A rejected window is exhausted. An overage window is recorded but does
/// not gate, and the top-level window adds a reading only for a window
/// `unifiedWindows` lacks, with its model scope.
#[test]
fn a_claude_rate_limit_event_marks_rejected_and_overage_windows() {
    let event = json!({
        "type": "rate_limit_event",
        "rate_limit_info": {
            "status": "rejected",
            "resetsAt": 1_791_561_600,
            "rateLimitType": "seven_day_sonnet",
            "utilization": 1.02,
            "unifiedWindows": {
                "five_hour": { "utilization": 0.5, "resetsAt": 1_791_525_000 },
                "seven_day_overage_included": { "utilization": 0.25, "resetsAt": 1_791_554_400 },
            },
        },
    });
    let stdout = format!("{event}\n{}", CLAUDE_STREAM.lines().last().unwrap());
    let (_, limits) = dispatch("claude", Provider::Claude, &stdout, &[]);
    let windows: Vec<_> = limits
        .iter()
        .map(|reading| {
            (
                reading.window.as_deref().unwrap(),
                reading.model.as_deref(),
                reading.used_percent,
                reading.exhausted,
                reading.gating,
            )
        })
        .collect();
    assert_eq!(
        windows,
        [
            ("five_hour", None, Some(50.0), false, true),
            ("seven_day_overage_included", None, Some(25.0), false, false),
            ("seven_day_sonnet", Some("sonnet"), Some(102.0), true, true),
        ]
    );
}
