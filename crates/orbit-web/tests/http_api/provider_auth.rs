//! Drain readiness shows every provider reading's age and the authentication
//! failures that no success of that provider on that host has cleared. A
//! success clears a failure only when it started after the failure.

use chrono::{Duration, Utc};
use orbit_core::{JobRun, JobRunState};
use orbit_types::task::ExecutionLocation;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{ActivityCrewDraw, JobRunStep, JobTargetType, PipelineState};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "ws_http_fixture";

#[test]
fn readiness_shows_stale_readings_and_clears_a_host_auth_failure_after_success() {
    isolated(
        "provider_auth::readiness_shows_stale_readings_and_clears_a_host_auth_failure_after_success",
        || {
            let fixture = claude_fixture();
            let now = Utc::now();
            fixture
                .runtime
                .record_provider_limit(&reading(
                    "codex",
                    "seven_day",
                    92.0,
                    now - Duration::days(3),
                    now + Duration::days(4),
                ))
                .unwrap();
            fixture
                .runtime
                .record_provider_limit(&reading(
                    "claude",
                    "five_hour",
                    40.0,
                    now,
                    now + Duration::hours(5),
                ))
                .unwrap();

            let auth_early = now - Duration::minutes(10);
            let auth_late = now - Duration::minutes(9);
            let capacity_at = now - Duration::minutes(8);
            let other_host_at = now - Duration::minutes(7);
            save_failed(
                &fixture,
                "jrun-auth-early",
                "dk-server-2",
                auth_early,
                "provider_unavailable",
                "[provider_unavailable] provider=claude token expired",
            );
            save_failed(
                &fixture,
                "jrun-auth-late",
                "dk-server-2",
                auth_late,
                "provider_unavailable",
                "[provider_unavailable] provider=claude token expired",
            );
            save_failed(
                &fixture,
                "jrun-capacity",
                "dk-server-2",
                capacity_at,
                "provider_capacity",
                "[provider_capacity] provider=claude overloaded",
            );
            save_failed(
                &fixture,
                "jrun-auth-other-host",
                "dk-server-1",
                other_host_at,
                "provider_unavailable",
                "[provider_unavailable] provider=claude token expired",
            );

            let server = fixture.server(false);
            let uri = format!("/api/workflows/auto/readiness?workspace={WS}");
            let before = json_ok(server.get(&uri));
            assert_reading(&before["provider_limits"], "codex", true, 2 * 24 * 3600);
            assert_reading(&before["provider_limits"], "claude", false, 0);
            let auth = auth_item(&before["provider_auth_failures"], "dk-server-2", "claude");
            let runs = run_ids(auth);
            assert_eq!(
                runs,
                vec!["jrun-auth-early".to_string(), "jrun-auth-late".to_string()],
                "the standing failure names both runs, oldest first, and not the capacity run: {auth}"
            );
            let fix = auth["fix_command"].as_str().unwrap_or_default();
            assert!(
                fix.contains("claude auth login"),
                "the fix command tells the operator how to renew the login: {fix}"
            );
            assert!(
                auth_item(&before["provider_auth_failures"], "dk-server-1", "claude")["runs"]
                    .as_array()
                    .is_some_and(|runs| {
                        runs.iter()
                            .any(|run| run["run_id"] == "jrun-auth-other-host")
                    }),
                "another host's failure stays visible: {}",
                before["provider_auth_failures"]
            );

            save_success(
                &fixture,
                "jrun-auth-cleared",
                "dk-server-2",
                now - Duration::seconds(30),
                now - Duration::seconds(30),
            );
            let after = json_ok(server.get(&uri));
            assert!(
                !after["provider_auth_failures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["host"] == "dk-server-2" && item["provider"] == "claude"),
                "a later successful claude run on that host clears it: {}",
                after["provider_auth_failures"]
            );
            assert!(
                auth_item(&after["provider_auth_failures"], "dk-server-1", "claude")["runs"]
                    .as_array()
                    .is_some_and(|runs| {
                        runs.iter()
                            .any(|run| run["run_id"] == "jrun-auth-other-host")
                    }),
                "the other host's failure is not cleared by this host's success: {}",
                after["provider_auth_failures"]
            );
            assert_reading(&after["provider_limits"], "claude", false, 0);
        },
    );
}

#[test]
fn host_resources_carry_the_hosts_provider_readings_for_the_top_bar() {
    isolated(
        "provider_auth::host_resources_carry_the_hosts_provider_readings_for_the_top_bar",
        || {
            let fixture = claude_fixture();
            let server = fixture.server(false);
            let empty = json_ok(server.get("/api/host/resources"));
            assert_eq!(
                empty["provider_limits"],
                serde_json::json!([]),
                "a host with no readings answers an empty list, not a missing field"
            );

            let now = Utc::now();
            fixture
                .runtime
                .record_provider_limit(&reading(
                    "codex",
                    "seven_day",
                    92.0,
                    now - Duration::days(3),
                    now + Duration::days(4),
                ))
                .unwrap();
            fixture
                .runtime
                .record_provider_limit(&reading(
                    "claude",
                    "five_hour",
                    40.0,
                    now,
                    now + Duration::hours(5),
                ))
                .unwrap();
            // The workspace query is ignored: the bar names the host, not a workspace.
            let sample = json_ok(server.get("/api/host/resources?workspace=missing"));
            assert!(
                sample["cpu"].is_object(),
                "the resource sample is still served: {sample}"
            );
            assert_reading(&sample["provider_limits"], "codex", true, 2 * 24 * 3600);
            assert_reading(&sample["provider_limits"], "claude", false, 0);
            let claude = sample["provider_limits"]
                .as_array()
                .and_then(|items| items.iter().find(|item| item["provider"] == "claude"))
                .unwrap();
            assert_eq!(claude["window"], "five_hour");
            assert_eq!(claude["used_percent"], 40.0);
            assert_eq!(claude["gated"], false);
            let pressures = sample["provider_pressures"].as_array().unwrap();
            let claude_pressure = pressures
                .iter()
                .find(|item| item["provider"] == "claude")
                .unwrap();
            assert_eq!(
                claude_pressure["pressure"], 1.0,
                "five-hour windows are neutral"
            );
            assert!(claude_pressure["pressure_window"].is_null());
            let codex_pressure = pressures
                .iter()
                .find(|item| item["provider"] == "codex")
                .unwrap();
            assert!((codex_pressure["pressure"].as_f64().unwrap() - 0.14).abs() < 0.001);
            assert_eq!(codex_pressure["pressure_window"]["window"], "seven_day");
            assert!(sample["crew_pressures"].is_array());
            assert!(claude["threshold"].is_number() || claude["threshold"] == "off");
        },
    );
}

/// Runs the shipped dashboard in Chromium against the real server: the top bar
/// shows the host's stored readings on every destination. Browser dependencies
/// and the evidence path are supplied explicitly.
#[test]
#[ignore = "requires ORBIT_PLAYWRIGHT_MODULE and ORBIT_PROVIDER_USAGE_BROWSER_EVIDENCE_DIR"]
fn dashboard_top_bar_shows_provider_usage_on_every_tab() {
    isolated(
        "provider_auth::dashboard_top_bar_shows_provider_usage_on_every_tab",
        || {
            let fixture = Fixture::new();
            let global_path = fixture.global.join("config.toml");
            let mut global = std::fs::read_to_string(&global_path).unwrap();
            global.push_str(concat!(
                "\n[workflow]\ndefault_crew = \"opus\"\nprovider_limit_overrides = [\"codex:off\"]\n",
                "\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n",
                "\n[crews.codex_fixture]\nprovider = \"codex\"\nmodel = \"fixture\"\n",
            ));
            std::fs::write(global_path, global).unwrap();
            let now = Utc::now();
            for (provider, window, used, observed_at) in [
                ("codex", "primary", 100.0, now - Duration::days(3)),
                ("claude", "five_hour", 5.0, now),
                ("claude", "seven_day", 10.0, now),
                ("codex", "secondary", 0.0, now),
            ] {
                fixture
                    .runtime
                    .record_provider_limit(&reading(
                        provider,
                        window,
                        used,
                        observed_at,
                        now + match window {
                            "seven_day" => Duration::hours(6),
                            "secondary" => Duration::days(7),
                            _ => Duration::days(4),
                        },
                    ))
                    .unwrap();
            }
            let server = fixture.server(false);
            let output = std::process::Command::new("node")
                .arg(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/http_api/dashboard_provider_usage_browser.mjs"),
                )
                .arg(
                    std::env::var_os("ORBIT_PLAYWRIGHT_MODULE")
                        .expect("prepared Playwright module"),
                )
                .arg(
                    std::env::var_os("ORBIT_PROVIDER_USAGE_BROWSER_EVIDENCE_DIR")
                        .expect("browser evidence directory"),
                )
                .arg(&server.origin)
                .output()
                .expect("launch provider usage browser fixture");
            assert!(
                output.status.success(),
                "provider usage browser check failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        },
    );
}

#[test]
fn readiness_keeps_a_failure_that_an_overlapping_success_did_not_clear() {
    isolated(
        "provider_auth::readiness_keeps_a_failure_that_an_overlapping_success_did_not_clear",
        || {
            let fixture = claude_fixture();
            let now = Utc::now();
            save_failed(
                &fixture,
                "jrun-auth",
                "dk-server-2",
                now - Duration::minutes(10),
                "provider_unavailable",
                "[provider_unavailable] provider=claude token expired",
            );
            // Started before the failure and finished after it: the run was
            // already using the credentials that then failed, so it does not
            // show that the login works now.
            save_success(
                &fixture,
                "jrun-overlap",
                "dk-server-2",
                now - Duration::minutes(15),
                now - Duration::minutes(5),
            );

            let server = fixture.server(false);
            let uri = format!("/api/workflows/auto/readiness?workspace={WS}");
            let overlapped = json_ok(server.get(&uri));
            let auth = auth_item(
                &overlapped["provider_auth_failures"],
                "dk-server-2",
                "claude",
            );
            assert_eq!(
                run_ids(auth),
                vec!["jrun-auth".to_string()],
                "a success that started before the failure leaves it standing: {auth}"
            );

            save_success(
                &fixture,
                "jrun-after",
                "dk-server-2",
                now - Duration::minutes(2),
                now - Duration::minutes(1),
            );
            let cleared = json_ok(server.get(&uri));
            assert!(
                !cleared["provider_auth_failures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item["host"] == "dk-server-2" && item["provider"] == "claude"),
                "a success that started after the failure clears it: {}",
                cleared["provider_auth_failures"]
            );
        },
    );
}

#[test]
fn readiness_counts_recorded_draws_instead_of_the_task_crew() {
    isolated(
        "provider_auth::readiness_counts_recorded_draws_instead_of_the_task_crew",
        || {
            let fixture = claude_fixture();
            let now = Utc::now();
            save_failed(
                &fixture,
                "jrun-auth",
                "dk-server-2",
                now - Duration::minutes(10),
                "provider_unavailable",
                "[provider_unavailable] provider=claude token expired",
            );
            let server = fixture.server(false);
            let uri = format!("/api/workflows/auto/readiness?workspace={WS}");
            for (job, key) in [
                ("task_pilot_pipeline", "workflow.low_complexity_crews"),
                (
                    "blocked_task_recovery_pipeline",
                    "workflow.final_recovery_crews",
                ),
                ("finding_verification_pipeline", ""),
            ] {
                for drawn in [Some("codex_fixture"), None, Some("opus")] {
                    let mut run = located(
                        "dk-server-2",
                        now - Duration::minutes(2),
                        JobRunState::Success,
                        Some("opus"),
                    );
                    run.run_id = format!("jrun-{job}");
                    run.job_id = job.into();
                    let mut state = PipelineState::new(run.run_id.clone(), job.into(), json!({}));
                    if let Some(crew) = drawn {
                        if key.is_empty() {
                            state.pipeline = json!({
                                "prepare": {"partitions": [{"crew": crew}]},
                                "verdict_results": [{"tasks": []}],
                            });
                        } else {
                            state.activity_crew_draws.insert(
                                key.into(),
                                ActivityCrewDraw {
                                    crew: crew.into(),
                                    source: key.into(),
                                    eligible_pool: Vec::new(),
                                },
                            );
                        }
                    }
                    fixture
                        .runtime
                        .sqlite_store()
                        .unwrap()
                        .upsert_job_run_for_workspace(
                            &fixture.runtime.workspace_id().unwrap(),
                            &run,
                            Some(&state),
                        )
                        .unwrap();
                    let readiness = json_ok(server.get(&uri));
                    let standing = readiness["provider_auth_failures"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["host"] == "dk-server-2" && item["provider"] == "claude");
                    assert_eq!(
                        standing,
                        drawn != Some("opus"),
                        "{job} with draw {drawn:?} must clear only the drawn provider: {readiness}"
                    );
                    // Remove this success from the next case's seven-day projection.
                    run.created_at = now - Duration::days(8);
                    fixture.save_run(&run);
                }
            }
        },
    );
}

/// A fixture whose `opus` crew runs on claude, so runs that resolve to it
/// count as claude runs.
fn claude_fixture() -> Fixture {
    let fixture = Fixture::new();
    let global_path = fixture.global.join("config.toml");
    let mut global = std::fs::read_to_string(&global_path).unwrap();
    global.push_str(concat!(
        "\n[workflow]\ndefault_crew = \"opus\"\n",
        "\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n",
    ));
    global.push_str("\n[crews.codex_fixture]\nprovider = \"codex\"\nmodel = \"fixture\"\n");
    std::fs::write(global_path, global).unwrap();
    fixture
}

fn reading(
    provider: &str,
    window: &str,
    used_percent: f64,
    observed_at: chrono::DateTime<Utc>,
    resets_at: chrono::DateTime<Utc>,
) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.into(),
        account: None,
        model: None,
        window: Some(window.into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(resets_at),
        used_percent: Some(used_percent),
        window_minutes: Some(if window == "seven_day" || window == "secondary" {
            10080
        } else {
            300
        }),
        gating: true,
        partial: false,
        observed_at,
        run_id: None,
        crew: None,
        detail: String::new(),
    }
}

fn located(
    host: &str,
    at: chrono::DateTime<Utc>,
    state: JobRunState,
    crew: Option<&str>,
) -> JobRun {
    JobRun {
        run_id: String::new(),
        job_id: "fixture-provider".into(),
        attempt: 1,
        state,
        scheduled_at: at,
        created_at: at,
        started_at: Some(at),
        finished_at: Some(at),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: crew.map(str::to_string),
        crew_model: None,
        steps: Vec::new(),
        executed_on: Some(ExecutionLocation {
            machine_id: format!("mid-{host}"),
            machine_name: Some(host.into()),
        }),
    }
}

fn save_failed(
    fixture: &Fixture,
    run_id: &str,
    host: &str,
    at: chrono::DateTime<Utc>,
    error_code: &str,
    error_message: &str,
) {
    let mut run = located(host, at, JobRunState::Failed, Some("opus"));
    run.run_id = run_id.into();
    fixture.save_run(&run);
    let step = JobRunStep {
        lock_wait_ms: None,
        step_index: 0,
        target_type: JobTargetType::Activity,
        target_id: "fixture".into(),
        started_at: Some(at),
        finished_at: Some(at),
        duration_ms: None,
        exit_code: Some(1),
        agent_response_json: None,
        state: JobRunState::Failed,
        error_code: Some(error_code.into()),
        error_message: Some(error_message.into()),
    };
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_step_for_workspace(&fixture.runtime.workspace_id().unwrap(), run_id, &step)
        .unwrap();
}

fn save_success(
    fixture: &Fixture,
    run_id: &str,
    host: &str,
    started: chrono::DateTime<Utc>,
    finished: chrono::DateTime<Utc>,
) {
    let mut run = located(host, started, JobRunState::Success, Some("opus"));
    run.finished_at = Some(finished);
    run.run_id = run_id.into();
    fixture.save_run(&run);
}

fn assert_reading(readings: &Value, provider: &str, stale: bool, min_age_seconds: i64) {
    let reading = readings
        .as_array()
        .and_then(|items| items.iter().find(|item| item["provider"] == provider))
        .unwrap_or_else(|| panic!("missing {provider} reading in {readings}"));
    assert_eq!(
        reading["stale"], stale,
        "{provider} stale marker: {reading}"
    );
    let age = reading["age_seconds"].as_i64().unwrap_or(-1);
    if stale {
        assert!(
            age >= min_age_seconds,
            "{provider} age {age}s is not at least {min_age_seconds}s: {reading}"
        );
    } else {
        assert!(
            (0..120).contains(&age),
            "a fresh {provider} reading reports a small age, got {age}s: {reading}"
        );
    }
}

fn auth_item<'a>(items: &'a Value, host: &str, provider: &str) -> &'a Value {
    items
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|item| item["host"] == host && item["provider"] == provider)
        })
        .unwrap_or_else(|| panic!("missing auth failure for {provider} on {host} in {items}"))
}

fn run_ids(item: &Value) -> Vec<String> {
    item["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["run_id"].as_str().unwrap().to_string())
        .collect()
}
