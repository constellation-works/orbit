//! [ORB-14695] A provider whose account hit a usage limit, run through
//! `dispatch_v2_activity` and `execute_job_with_resume` against fake provider
//! CLIs that print recorded limit texts.
//!
//! Only text the provider wrote about itself types the failure as
//! `[provider_limit]`: stderr, the terminal error, or its own failure frames.
//! The typed failure carries the reset the provider reported, the run records
//! the limit in its host's store, and step and final recovery are skipped.

use chrono::{DateTime, Duration, Local, TimeZone, Timelike, Utc};
use orbit_engine::{V2DispatchInput, dispatch_v2_activity};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::activity_job::{ActivityV2Spec, Provider};
use orbit_types::workflow::{
    ProviderFailureClass, ProviderLimitFailure, failed_provider, is_provider_limit,
    is_provider_unavailable,
};
use serde_json::json;

use super::provider_capacity::{
    CapacityHost, FakeProvider, SUCCESS_ENVELOPE, agent_spec, failure_message, implementation_job,
    run_job, writer,
};

/// What Antigravity wrote for crew `gemini-flash` (runs
/// jrun-20260920-0742-c19 and -c21): its terminal `ERROR` result on stdout.
const ANTIGRAVITY_QUOTA: &str = "Individual quota reached. Please upgrade your subscription to \
     increase your limits. Resets in 1h37m37s.";

fn antigravity_quota_stdout() -> String {
    format!(
        r#"{{"status":"ERROR","response":"","error":{}}}"#,
        serde_json::to_string(ANTIGRAVITY_QUOTA).unwrap()
    )
}

/// Gemini CLI's quota error (ORB-10814), on stderr.
const GEMINI_QUOTA: &str = "TerminalQuotaError: You exceeded your current quota, please check \
     your plan and billing details.";

/// codex-cli 0.161.0's limit message for one model, with an absolute reset
/// on another day.
const CODEX_LIMIT: &str = "You've hit your usage limit for gpt-5.1-codex-max. Visit \
     https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Oct 9th, \
     2026 3:42 PM.";

/// The same message on the day it resets, with no model.
const CODEX_LIMIT_TODAY: &str = "You've hit your usage limit. Upgrade to Pro \
     (https://chatgpt.com/explore/pro) or try again at 3:42 PM.";

/// Codex's own `error` and `turn.failed` frames after the turn's tool traffic.
fn codex_limit_frames(message: &str) -> String {
    let message = serde_json::to_string(message).unwrap();
    format!(
        r#"{{"type":"item.completed","item":{{"id":"item_1","type":"command_execution","command":"cargo test","aggregated_output":"","exit_code":0,"status":"completed"}}}}
{{"type":"error","message":{message}}}
{{"type":"turn.failed","error":{{"message":{message}}}}}"#
    )
}

/// Claude Code 2.1.294's limit result.
const CLAUDE_LIMIT: &str = "You've hit your limit · resets 3pm (America/Los_Angeles)";

/// grok 1.0.46's HTTP failure, on stderr.
const GROK_LIMIT: &str = "Error: Too Many Requests (429): rate limit exceeded for this account";

/// An agent's declared envelope failure that quotes a GitHub rate limit (box
/// run jrun-20261003-0630-c2): the work's report, not the provider's.
const GITHUB_RATE_LIMIT_ENVELOPE: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"partial_scan","message":"partial_scan: GitHub rate-limit exhaustion; Too Many Requests (429) from api.github.com; You've hit your usage limit"}}"#;

struct Case {
    binary: &'static str,
    provider: Provider,
    stdout: String,
    stderr: &'static str,
    exit_code: i32,
    limit: bool,
}

fn case(
    binary: &'static str,
    provider: Provider,
    stdout: String,
    stderr: &'static str,
    exit_code: i32,
    limit: bool,
) -> Case {
    Case {
        binary,
        provider,
        stdout,
        stderr,
        exit_code,
        limit,
    }
}

/// Dispatch one fake provider and return its failure message and the limits
/// the run recorded.
fn dispatch(
    index: usize,
    case: &Case,
) -> (bool, Option<String>, Vec<ProviderLimitObservation>, String) {
    let fake = FakeProvider::new(case.binary, &case.stdout, case.stderr, case.exit_code, "");
    let run_id = format!("limit-{index}");
    let audit = tempfile::tempdir().unwrap();
    let host = CapacityHost::new(&fake.path, audit.path());
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "limit_fixture",
        spec: &ActivityV2Spec::AgentLoop(agent_spec(case.provider)),
        fs_profile: None,
        input: json!({ "prompt": "implement" }),
        audit: writer(audit.path(), &run_id),
        run_id: &run_id,
        host: Some(&host),
    })
    .unwrap();
    let limits = host.limits.lock().unwrap().clone();
    (outcome.success, outcome.message, limits, run_id)
}

/// The limit a typed failure carries, after asserting the failure is typed.
fn typed_limit(message: Option<&str>, provider: &str) -> ProviderLimitFailure {
    let message = message.expect("a failed step has a message");
    assert!(is_provider_limit(None, Some(message)), "{message}");
    assert!(
        is_provider_unavailable(None, Some(message)),
        "a limit is an unavailability, as capacity is: {message}"
    );
    assert_eq!(
        ProviderFailureClass::of(None, Some(message)),
        Some(ProviderFailureClass::Limit),
        "{message}"
    );
    assert_eq!(failed_provider(message), Some(provider), "{message}");
    ProviderLimitFailure::from_text(message).expect("the failure carries the limit")
}

/// Every recorded provider limit text types the failure, and nothing the
/// agent wrote does.
#[test]
fn usage_limits_are_typed_only_from_text_the_failed_provider_wrote() {
    let envelope_answer = format!(
        r#"{{"type":"item.completed","item":{{"type":"agent_message","text":{}}}}}"#,
        serde_json::to_string(SUCCESS_ENVELOPE).unwrap()
    );
    let declared_failure = format!(
        r#"{{"type":"item.completed","item":{{"type":"agent_message","text":{}}}}}"#,
        serde_json::to_string(GITHUB_RATE_LIMIT_ENVELOPE).unwrap()
    );
    let cases = vec![
        case(
            "agy",
            Provider::Antigravity,
            antigravity_quota_stdout(),
            "",
            1,
            true,
        ),
        case(
            "gemini",
            Provider::Gemini,
            String::new(),
            GEMINI_QUOTA,
            1,
            true,
        ),
        case(
            "codex",
            Provider::Codex,
            codex_limit_frames(CODEX_LIMIT),
            "",
            1,
            true,
        ),
        case(
            "claude",
            Provider::Claude,
            format!(
                r#"{{"type":"result","subtype":"success","is_error":true,"result":{}}}"#,
                serde_json::to_string(CLAUDE_LIMIT).unwrap()
            ),
            "",
            1,
            true,
        ),
        case("grok", Provider::Grok, String::new(), GROK_LIMIT, 1, true),
        // The agent declared its own failure over a GitHub rate limit.
        case("codex", Provider::Codex, declared_failure, "", 0, false),
        case(
            "claude",
            Provider::Claude,
            GITHUB_RATE_LIMIT_ENVELOPE.to_string(),
            "",
            1,
            false,
        ),
        // A tool the agent ran printed a limit.
        case(
            "codex",
            Provider::Codex,
            format!(
                r#"{{"type":"item.completed","item":{{"type":"command_execution","command":"gh api","aggregated_output":{},"exit_code":1,"status":"failed"}}}}"#,
                serde_json::to_string(GROK_LIMIT).unwrap()
            ),
            "",
            1,
            false,
        ),
        // Reported mid-turn, then the turn finished.
        case(
            "codex",
            Provider::Codex,
            format!("{}\n{envelope_answer}", codex_limit_frames(CODEX_LIMIT)),
            "",
            0,
            false,
        ),
    ];
    for (index, case) in cases.iter().enumerate() {
        let before = Utc::now();
        let (success, message, limits, run_id) = dispatch(index, case);
        let after = Utc::now();
        let label = format!("case {index} ({} exit {})", case.binary, case.exit_code);
        assert_eq!(
            is_provider_limit(None, message.as_deref()),
            case.limit,
            "{label}: {message:?}"
        );
        if !case.limit {
            assert!(
                limits.is_empty(),
                "{label}: nothing is recorded: {limits:?}"
            );
            continue;
        }
        assert!(!success, "{label}");
        let provider = failed_provider(message.as_deref().unwrap())
            .expect("the failure names its provider")
            .to_string();
        let limit = typed_limit(message.as_deref(), &provider);
        let [observation] = limits.as_slice() else {
            panic!("{label}: one observation per limit failure: {limits:?}");
        };
        assert_eq!(observation.provider, provider, "{label}");
        assert!(observation.exhausted, "{label}");
        assert_eq!(observation.source, ProviderLimitSource::Error, "{label}");
        assert_eq!(observation.resets_at, limit.resets_at, "{label}");
        assert_eq!(observation.model, limit.model, "{label}");
        assert_eq!(observation.run_id.as_deref(), Some(run_id.as_str()));
        assert!(
            observation.observed_at >= before && observation.observed_at <= after,
            "{label}: {observation:?}"
        );
        assert!(!observation.detail.is_empty(), "{label}");

        match case.binary {
            "agy" => {
                let reset = Duration::hours(1) + Duration::minutes(37) + Duration::seconds(37);
                let resets_at = limit.resets_at.expect("Antigravity said when it resets");
                assert!(
                    resets_at >= before + reset - Duration::seconds(1)
                        && resets_at <= after + reset,
                    "{label}: resets 1h37m37s after the failure: {resets_at}"
                );
                assert!(
                    message.as_deref().unwrap().contains(ANTIGRAVITY_QUOTA),
                    "the operator sees the provider's own words"
                );
            }
            "codex" => {
                let expected = Local
                    .with_ymd_and_hms(2026, 10, 9, 15, 42, 0)
                    .single()
                    .unwrap()
                    .with_timezone(&Utc);
                assert_eq!(limit.resets_at, Some(expected), "{label}");
                assert_eq!(limit.model.as_deref(), Some("gpt-5.1-codex-max"));
            }
            "claude" => {
                let resets_at = limit.resets_at.expect("Claude said when it resets");
                let pacific = resets_at.with_timezone(&chrono_tz::America::Los_Angeles);
                assert_eq!((pacific.hour(), pacific.minute()), (15, 0), "{label}");
                assert!(
                    resets_at > before && resets_at <= after + Duration::days(1),
                    "{label}: the next 3pm Pacific: {resets_at}"
                );
            }
            _ => assert_eq!(
                limit.resets_at, None,
                "{label}: no reset is invented: {message:?}"
            ),
        }
    }
}

/// Codex's same-day reset is the host's local time today; Claude's
/// `rate_limit_info` names the window and its epoch reset, and fails the turn
/// even on exit 0.
#[test]
fn reported_resets_and_windows_are_read_from_the_providers_own_words() {
    let (_, message, _, _) = dispatch(
        0,
        &case(
            "codex",
            Provider::Codex,
            codex_limit_frames(CODEX_LIMIT_TODAY),
            "",
            1,
            true,
        ),
    );
    let limit = typed_limit(message.as_deref(), "codex");
    let local = limit
        .resets_at
        .expect("codex said when it resets")
        .with_timezone(&Local);
    assert_eq!((local.hour(), local.minute()), (15, 42), "{message:?}");
    assert_eq!(local.date_naive(), Local::now().date_naive());
    assert_eq!(limit.model, None, "the limit names no model");

    let resets_at = DateTime::from_timestamp(1_791_561_600, 0).unwrap();
    let stdout = format!(
        "{}\n{}",
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1791561600,"rateLimitType":"seven_day_opus"}}"#,
        r#"{"type":"result","subtype":"success","is_error":true,"result":"You've reached your Opus limit"}"#
    );
    let (success, message, limits, _) =
        dispatch(1, &case("claude", Provider::Claude, stdout, "", 0, true));
    assert!(!success, "a limit result fails the turn on exit 0");
    let limit = typed_limit(message.as_deref(), "claude");
    assert_eq!(limit.window.as_deref(), Some("seven_day_opus"));
    assert_eq!(limit.model.as_deref(), Some("opus"));
    assert_eq!(limit.resets_at, Some(resets_at));
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].window.as_deref(), Some("seven_day_opus"));
    assert_eq!(limits[0].resets_at, Some(resets_at));
}

/// The incident runs spent a step recovery on a quota. Now the provider is
/// invoked once, neither recovery runs, and the failure handoff gets the typed
/// code with the candidate intact.
#[test]
fn a_usage_limit_skips_recovery_and_keeps_the_candidate() {
    let worktree = tempfile::tempdir().unwrap();
    let candidate = worktree.path().join("candidate.rs");
    let fake = FakeProvider::new(
        "agy",
        &antigravity_quota_stdout(),
        "",
        1,
        &format!("echo partial > '{}'", candidate.display()),
    );
    let host = CapacityHost::new(&fake.path, worktree.path());
    let run = run_job(
        &implementation_job(agent_spec(Provider::Antigravity)),
        &host,
        "limit-run",
    );

    let message = failure_message(&run.outcome);
    typed_limit(Some(&message), "antigravity");
    assert_eq!(
        fake.invocations(),
        1,
        "the limited account is not asked again"
    );
    assert!(host.calls("step_fix").is_empty(), "no step recovery runs");
    assert_eq!(
        *host.final_recovery_admissions.lock().unwrap(),
        0,
        "final recovery is not admitted"
    );
    assert!(host.calls("decide").is_empty(), "no final recovery runs");
    let handoff = host.calls("handoff");
    assert_eq!(handoff.len(), 1, "the failure handoff keeps the candidate");
    assert_eq!(handoff[0]["error_code"], "provider_limit", "{handoff:?}");
    assert_eq!(
        std::fs::read_to_string(&candidate).unwrap().trim(),
        "partial",
        "the worktree still holds the partial candidate"
    );
    assert_eq!(
        host.limits.lock().unwrap().len(),
        1,
        "the limit is recorded"
    );
}
