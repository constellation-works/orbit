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
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use chrono::{DateTime, Local, Utc};
use orbit_engine::{
    DispatchError, DispatchOutcome, ResolvedCliExecutor, RuntimeHost, V2DispatchInput,
    dispatch_v2_activity,
};
use orbit_store::contracts::ProviderLimitStoreBackend;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource, TokenUsage};
use orbit_types::workflow::activity_job::{ActivityV2Spec, Provider};
use serde_json::{Value, json};

use super::provider_capacity::{CapacityHost, FakeProvider, agent_spec, outcome_writer};

const CODEX_STDOUT: &str = include_str!("fixtures/provider_usage/codex_exec_stdout.jsonl");
const CODEX_ROLLOUT: &str = include_str!("fixtures/provider_usage/codex_rollout.jsonl");
const CODEX_THREAD: &str = "01a11e3d-4d1d-7102-a520-fcba3210177d";
const CLAUDE_STREAM: &str = include_str!("fixtures/provider_usage/claude_stream.jsonl");
const GROK_BILLING: &str =
    include_str!("../../../orbit-agent/tests/provider_invocation/fixtures/grok_billing.json");

/// Exercise the real provider-limit persistence boundary during dispatch.
struct UsageHost {
    provider: CapacityHost,
    store: orbit_store::Store,
    cli_command: Option<String>,
    cli_args: Vec<String>,
}

impl orbit_engine::test_support::NullRuntimeHost for UsageHost {
    fn agent_subprocess_environment(&self, names: &[&str]) -> Vec<(String, String)> {
        RuntimeHost::agent_subprocess_environment(&self.provider, names)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        let mut executor = RuntimeHost::resolve_cli_executor(&self.provider, provider)?;
        if let Some(command) = &self.cli_command {
            executor.command = command.clone();
        }
        if !self.cli_args.is_empty() {
            executor.args = self.cli_args.clone();
        }
        Ok(executor)
    }

    fn record_provider_limit(
        &self,
        reading: &ProviderLimitObservation,
    ) -> Result<(), orbit_common::OrbitError> {
        self.store.record_provider_limit(reading).map(|_| ())
    }
}

/// Dispatch a fake `binary` that prints `stdout`, with `agent_env` in the
/// child's environment, and return the outcome and the limits recorded.
fn dispatch(
    binary: &str,
    provider: Provider,
    stdout: &str,
    agent_env: &[(&str, &Path)],
) -> (DispatchOutcome, Vec<ProviderLimitObservation>) {
    dispatch_with_prelude(binary, provider, stdout, agent_env, "")
}

fn dispatch_with_prelude(
    binary: &str,
    provider: Provider,
    stdout: &str,
    agent_env: &[(&str, &Path)],
    prelude: &str,
) -> (DispatchOutcome, Vec<ProviderLimitObservation>) {
    dispatch_with_prelude_and_launcher(binary, provider, stdout, agent_env, prelude, None, &[])
}

fn dispatch_with_prelude_and_launcher(
    binary: &str,
    provider: Provider,
    stdout: &str,
    agent_env: &[(&str, &Path)],
    prelude: &str,
    interpreter: Option<&str>,
    extra_args: &[String],
) -> (DispatchOutcome, Vec<ProviderLimitObservation>) {
    let fake = FakeProvider::with_usage_command(binary, stdout, "", 0, "", prelude);
    let audit = tempfile::tempdir().unwrap();
    let mut host = CapacityHost::new(&fake.path, audit.path());
    host.agent_env = agent_env
        .iter()
        .map(|(key, value)| (key.to_string(), value.display().to_string()))
        .collect();
    let (cli_command, cli_args) = if let Some(interpreter) = interpreter {
        let mut args = vec![fake.path.display().to_string()];
        args.extend(extra_args.iter().cloned());
        (Some(interpreter.to_string()), args)
    } else {
        (None, extra_args.to_vec())
    };
    let host = UsageHost {
        provider: host,
        store: orbit_store::Store::open_in_memory().unwrap(),
        cli_command,
        cli_args,
    };
    let mut spec = agent_spec(provider);
    spec.require_response_envelope = true;
    let outcome = dispatch_v2_activity(V2DispatchInput {
        activity_name: "usage_window_fixture",
        spec: &ActivityV2Spec::AgentLoop(spec),
        fs_profile: None,
        input: json!({ "prompt": "implement", "crew": "fixture-crew" }),
        audit: outcome_writer("usage-run"),
        run_id: "usage-run",
        host: Some(&host),
    })
    .unwrap();
    let limits = host.store.provider_limits().unwrap();
    (outcome, limits)
}

fn antigravity_records_native_quota_and_unavailable_readings_are_advisory() {
    let home = tempfile::tempdir().unwrap();
    let response = home.path().join("usage.json");
    let fixture = include_str!(
        "../../../orbit-agent/tests/provider_invocation/fixtures/antigravity_usage.json"
    );
    fs::write(&response, fixture).unwrap();
    // Same launcher and child's HOME, with a fixed native command and no prompt.
    let prelude = r#"
if [ "$1" = '--print' ]; then
  [ "$#" = 6 ] && [ "$2" = '/usage' ] && [ "$3" = '--output-format' ] && [ "$4" = 'json' ] && [ "$5" = '--print-timeout' ] && [ "$6" = '3s' ] || exit 9
  cat "$HOME/usage.json"
  exit $?
fi
"#;
    let success = json!({"event":"result", "result": {
        "status":"SUCCESS", "response":super::provider_capacity::SUCCESS_ENVELOPE,
        "usage":{"input_tokens":12, "output_tokens":4}
    }})
    .to_string();
    let env = [("HOME", home.path())];
    let run =
        |stdout: &str| dispatch_with_prelude("agy", Provider::Antigravity, stdout, &env, prelude);
    let (outcome, limits) = run(&success);
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(limits.len(), 9);
    for reading in &limits {
        assert_eq!(reading.provider, "antigravity");
        assert_eq!(reading.source, ProviderLimitSource::Event);
        assert_eq!(reading.run_id.as_deref(), Some("usage-run"));
        assert_eq!(reading.crew.as_deref(), Some("fixture-crew"));
        assert!(
            reading.used_percent.is_some()
                && reading.window_minutes.is_some()
                && reading.resets_at.is_some()
        );
    }
    let flash = limits
        .iter()
        .find(|reading| {
            reading.model.as_deref() == Some("flash")
                && reading.window.as_deref() == Some("gemini-5h")
        })
        .unwrap();
    assert_eq!(flash.used_percent, Some(9.81));
    assert_eq!(flash.window_minutes, Some(300));
    assert_eq!(
        flash.resets_at,
        Some("2026-10-24T08:49:02Z".parse().unwrap())
    );
    let (failed, failed_limits) = run("no completion envelope");
    assert!(!failed.success);
    assert_eq!(failed_limits.len(), 9, "failed runs also record usage");
    for body in ["not json".to_string(), "{}".to_string(), "x".repeat(65537)] {
        fs::write(&response, body).unwrap();
        let (outcome, limits) = run(&success);
        assert!(
            outcome.success,
            "advisory quota read: {:?}",
            outcome.message
        );
        assert!(limits.is_empty());
    }
    fs::rename(&response, home.path().join("saved-usage.json")).unwrap();
    let (outcome, limits) = run(&success);
    assert!(
        outcome.success,
        "an unavailable quota command cannot fail a run"
    );
    assert!(limits.is_empty());
}

fn antigravity_preserves_launcher_prefix_arguments_in_quota_reads() {
    let home = tempfile::tempdir().unwrap();
    let response = home.path().join("usage.json");
    let fixture = include_str!(
        "../../../orbit-agent/tests/provider_invocation/fixtures/antigravity_usage.json"
    );
    fs::write(&response, fixture).unwrap();
    let prelude = r#"
if [ "$1" = '--print' ]; then
  [ "$#" = 6 ] && [ "$2" = '/usage' ] && [ "$3" = '--output-format' ] && [ "$4" = 'json' ] && [ "$5" = '--print-timeout' ] && [ "$6" = '3s' ] || exit 9
  cat "$HOME/usage.json"
  exit $?
fi
"#;
    let success = json!({"event":"result", "result": {
        "status":"SUCCESS", "response":super::provider_capacity::SUCCESS_ENVELOPE,
        "usage":{"input_tokens":12, "output_tokens":4}
    }})
    .to_string();
    let env = [("HOME", home.path())];
    // Configured executor uses an interpreter (/bin/sh) with the provider script
    // prefix as well as turn-specific flags that must be replaced by quota flags.
    let run = |stdout: &str| {
        dispatch_with_prelude_and_launcher(
            "agy-wrapper.sh",
            Provider::Antigravity,
            stdout,
            &env,
            prelude,
            Some("/bin/sh"),
            &[
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--print-timeout".to_string(),
                "10m".to_string(),
            ],
        )
    };
    let (outcome, limits) = run(&success);
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(limits.len(), 9);
    for reading in &limits {
        assert_eq!(reading.provider, "antigravity");
        assert_eq!(reading.source, ProviderLimitSource::Event);
        assert_eq!(reading.run_id.as_deref(), Some("usage-run"));
        assert_eq!(reading.crew.as_deref(), Some("fixture-crew"));
        assert!(
            reading.used_percent.is_some()
                && reading.window_minutes.is_some()
                && reading.resets_at.is_some()
        );
    }
    let flash = limits
        .iter()
        .find(|reading| {
            reading.model.as_deref() == Some("flash")
                && reading.window.as_deref() == Some("gemini-5h")
        })
        .unwrap();
    assert_eq!(flash.used_percent, Some(9.81));
    assert_eq!(flash.window_minutes, Some(300));
    assert_eq!(
        flash.resets_at,
        Some("2026-10-24T08:49:02Z".parse().unwrap())
    );
    let (failed, failed_limits) = run("no completion envelope");
    assert!(!failed.success);
    assert_eq!(failed_limits.len(), 9, "failed runs also record usage");
    for body in ["not json".to_string(), "{}".to_string(), "x".repeat(65537)] {
        fs::write(&response, body).unwrap();
        let (outcome, limits) = run(&success);
        assert!(
            outcome.success,
            "advisory quota read: {:?}",
            outcome.message
        );
        assert!(limits.is_empty());
    }
    fs::rename(&response, home.path().join("saved-usage.json")).unwrap();
    let (outcome, limits) = run(&success);
    assert!(
        outcome.success,
        "an unavailable quota command cannot fail a run"
    );
    assert!(limits.is_empty());
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

/// The provider child selects the state root and PATH; a deterministic curl
/// double checks that the existing login travels only over stdin to the fixed
/// HTTPS account endpoint. No real login or external network is used.
fn grok_records_billing_and_unavailable_or_unrecognized_readings_are_advisory() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let curl = bin.path().join("curl");
    let response = bin.path().join("response");
    let requests = bin.path().join("requests");
    fs::write(
        &curl,
        format!(
            r#"#!/bin/sh
printf x >> '{}'
IFS= read -r authorization
IFS= read -r token_auth
[ "$authorization" = 'Authorization: Bearer fixture-token' ] || exit 9
[ "$token_auth" = 'X-XAI-Token-Auth: xai-grok-cli' ] || exit 9
[ "$1" = '--disable' ] || exit 9
case "$*" in *fixture-token*|*--location*) exit 9 ;; esac
case "$*" in *'https://cli-chat-proxy.grok.com/v1/billing?format=credits') ;; *) exit 9 ;; esac
cat '{}'
"#,
            requests.display(),
            response.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&curl, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap()
    );
    let auth = home.path().join("auth.json");
    let credential = json!({"https://auth.x.ai::fixture": {
        "key": "fixture-token", "oidc_issuer": "https://auth.x.ai"
    }});
    fs::write(&auth, credential.to_string()).unwrap();
    fs::write(&response, format!("{GROK_BILLING}\n200")).unwrap();
    let success =
        json!({"text": super::provider_capacity::SUCCESS_ENVELOPE, "stopReason": "end_turn"})
            .to_string();
    let env = [("GROK_HOME", home.path()), ("PATH", Path::new(&path))];
    let (outcome, limits) = dispatch("grok", Provider::Grok, &success, &env);
    assert!(outcome.success, "{:?}", outcome.message);
    let [reading] = limits.as_slice() else {
        panic!("one reported weekly window: {limits:?}")
    };
    assert_eq!(reading.provider, "grok");
    assert_eq!(reading.source, ProviderLimitSource::Event);
    assert_eq!(reading.window.as_deref(), Some("weekly"));
    assert_eq!(reading.used_percent, Some(2.0));
    assert_eq!(reading.window_minutes, Some(10080));
    assert_eq!(
        reading.resets_at,
        Some("2026-10-31T02:17:29.354Z".parse().unwrap())
    );
    assert_eq!(reading.run_id.as_deref(), Some("usage-run"));
    assert_eq!(reading.crew.as_deref(), Some("fixture-crew"));
    assert_eq!(reading.account, None);
    // A failed run still records its account window.
    let (failed, failed_limits) = dispatch("grok", Provider::Grok, "not an envelope", &env);
    assert!(!failed.success);
    assert_eq!(failed_limits[0].used_percent, Some(2.0));

    for body in ["{}\n200", "not JSON\n200", "{}\n401", "{}\n302", "{}"] {
        fs::write(&response, body).unwrap();
        let (outcome, limits) = dispatch("grok", Provider::Grok, &success, &env);
        assert!(
            outcome.success,
            "advisory billing {body}: {:?}",
            outcome.message
        );
        assert!(limits.is_empty(), "advisory billing {body}: {limits:?}");
    }
    fs::write(&response, format!("{GROK_BILLING}\n200")).unwrap();
    // Unavailable, malformed, ambiguous or injected credentials never send a
    // request and never fall back to billing-looking assistant text.
    let forged = json!({"text": GROK_BILLING, "stopReason": "end_turn"}).to_string();
    let requested = fs::read(&requests).unwrap().len();
    for auth in [
        "{}".to_string(),
        "not json".to_string(),
        json!({
            "https://auth.x.ai::one": {"key": "fixture-token", "oidc_issuer": "https://auth.x.ai"},
            "https://auth.x.ai::two": {"key": "fixture-token", "oidc_issuer": "https://auth.x.ai"}
        })
        .to_string(),
        json!({"https://accounts.x.ai/sign-in": {"key":"fixture-token\nInjected: bad"}})
            .to_string(),
    ] {
        fs::write(home.path().join("auth.json"), auth).unwrap();
        let (_, limits) = dispatch("grok", Provider::Grok, &forged, &env);
        assert!(
            limits.is_empty(),
            "untrusted credentials must record nothing"
        );
        assert_eq!(
            fs::read(&requests).unwrap().len(),
            requested,
            "untrusted credentials must not reach the transport"
        );
    }
    fs::write(&auth, credential.to_string()).unwrap();
    let key_path = Path::new("api-key-override");
    let override_env = [
        ("GROK_HOME", home.path()),
        ("PATH", Path::new(&path)),
        ("XAI_API_KEY", key_path),
    ];
    let (outcome, limits) = dispatch("grok", Provider::Grok, &success, &override_env);
    assert!(outcome.success);
    assert!(
        limits.is_empty(),
        "an API key must not inherit cached OAuth usage"
    );
    assert_eq!(fs::read(&requests).unwrap().len(), requested);
    fs::write(&curl, "#!/bin/sh\nexit 7\n").unwrap();
    let (outcome, limits) = dispatch("grok", Provider::Grok, &success, &env);
    assert!(
        outcome.success,
        "an unavailable transport must not fail the run"
    );
    assert!(limits.is_empty());
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn usage_window_contracts_1() {
    crate::git_fixture::isolated(module_path!(), "usage_window_contracts_1", || {
        crate::git_fixture::run_scenarios(&[
            (
                "antigravity_records_native_quota_and_unavailable_readings_are_advisory",
                antigravity_records_native_quota_and_unavailable_readings_are_advisory,
            ),
            (
                "antigravity_preserves_launcher_prefix_arguments_in_quota_reads",
                antigravity_preserves_launcher_prefix_arguments_in_quota_reads,
            ),
            (
                "grok_records_billing_and_unavailable_or_unrecognized_readings_are_advisory",
                grok_records_billing_and_unavailable_or_unrecognized_readings_are_advisory,
            ),
            (
                "a_codex_run_records_the_window_its_own_rollout_reports",
                a_codex_run_records_the_window_its_own_rollout_reports,
            ),
            (
                "a_claude_stream_records_both_windows_and_projects_as_its_result_alone",
                a_claude_stream_records_both_windows_and_projects_as_its_result_alone,
            ),
            (
                "a_claude_rate_limit_event_marks_rejected_and_overage_windows",
                a_claude_rate_limit_event_marks_rejected_and_overage_windows,
            ),
        ]);
    });
}
