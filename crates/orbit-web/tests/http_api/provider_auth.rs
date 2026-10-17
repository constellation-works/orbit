//! Drain readiness shows every provider reading's age and the authentication
//! failures a later success of that provider on that host has not cleared.

use chrono::{Duration, Utc};
use orbit_core::{JobRun, JobRunState};
use orbit_types::task::ExecutionLocation;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{JobRunStep, JobTargetType};
use serde_json::Value;

use super::support::{Fixture, isolated, json_ok};

const WS: &str = "ws_http_fixture";

#[test]
fn readiness_shows_stale_readings_and_clears_a_host_auth_failure_after_success() {
    isolated(
        "provider_auth::readiness_shows_stale_readings_and_clears_a_host_auth_failure_after_success",
        || {
            let fixture = Fixture::new();
            let global_path = fixture.global.join("config.toml");
            let mut global = std::fs::read_to_string(&global_path).unwrap();
            global.push_str(concat!(
                "\n[workflow]\ndefault_crew = \"opus\"\n",
                "\n[crews.opus]\nprovider = \"claude\"\nmodel = \"opus\"\n",
            ));
            std::fs::write(global_path, global).unwrap();

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

fn reading(
    provider: &str,
    window: &str,
    used_percent: f64,
    observed_at: chrono::DateTime<Utc>,
    resets_at: chrono::DateTime<Utc>,
) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.into(),
        model: None,
        window: Some(window.into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(resets_at),
        used_percent: Some(used_percent),
        window_minutes: Some(300),
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

fn save_success(fixture: &Fixture, run_id: &str, host: &str, at: chrono::DateTime<Utc>) {
    let mut run = located(host, at, JobRunState::Success, Some("opus"));
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
