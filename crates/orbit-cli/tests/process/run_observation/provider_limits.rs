//! [ORB-14698] One seeded provider-limit store, read through every CLI
//! surface: `run readiness` text and JSON, a pull drain's `run show` crew
//! window, and the `doctor` and `doctor providers` provider-limits rows.
use super::*;
use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{PipelineState, PullCrewPreflight};
use toml_edit::{DocumentMut, value};

/// Claude crew `opus`, Gemini crew `gem` beside the fixture's Codex `sol`;
/// the system and review lanes run as `opus`.
fn configure_crews(fixture: &Fixture) {
    let path = fixture.home.join(".orbit/config.toml");
    let mut config: DocumentMut = fs::read_to_string(&path).unwrap().parse().unwrap();
    for (name, provider, model) in [
        ("opus", "claude", "opus-model"),
        ("gem", "gemini", "gem-model"),
        ("sonnet", "claude", "sonnet-model"),
        ("credits", "claude", "opus-model"),
        ("ledger", "grok", "grok-model"),
        ("agy-flash", "antigravity", "gemini-3.8-flash-high"),
        ("error", "claude", "opus-model"),
        ("lapsed", "claude", "opus-model"),
        ("no-reset", "claude", "opus-model"),
        ("overage", "claude", "opus-model"),
    ] {
        config["crews"][name]["enabled"] = value(true);
        config["crews"][name]["provider"] = value(provider);
        config["crews"][name]["model"] = value(model);
        config["crews"][name]["backend"] = value("cli");
    }
    for account in ["credits", "error", "lapsed", "no-reset", "overage"] {
        config["crews"][account]["env"] = value(toml_edit::Array::from_iter([
            "ANTHROPIC_API_KEY=PRESSURE_FIXTURE_KEY",
        ]));
    }
    config["workflow"]["provider_limit_budgets"] =
        value(toml_edit::Array::from_iter(["grok:100usd/7d"]));
    config["workflow"]["purpose_crews"]["review"] = value("opus");
    fs::write(&path, config.to_string()).unwrap();
}

fn reading(provider: &str, used_percent: f64, resets_in: Duration) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.into(),
        account: None,
        model: None,
        window: Some("five_hour".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(Utc::now() + resets_in),
        used_percent: Some(used_percent),
        window_minutes: Some(300),
        gating: true,
        partial: false,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    }
}

fn doctor_row<'a>(rows: &'a Value, check: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["check"] == check)
        .unwrap_or_else(|| panic!("doctor has no {check} row: {rows:#}"))
}

#[test]
fn native_provider_usage_events_are_visible_in_readiness_and_weekly_pressure() {
    if !isolated_run_observation(
        "run_observation::provider_limits::native_provider_usage_events_are_visible_in_readiness_and_weekly_pressure",
    ) {
        return;
    }
    let fixture = Fixture::init();
    configure_crews(&fixture);
    let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
        orbit_core::runtime::OrbitRuntimeRoots {
            global_root: fixture.home.join(".orbit"),
            shared_root: fixture.work.join(".orbit"),
            local_root: fixture.work.join(".orbit"),
        },
    )
    .unwrap();
    for (provider, window, scope) in [
        ("grok", "weekly", None),
        ("antigravity", "gemini-weekly", Some("flash")),
    ] {
        let mut observation = reading(provider, 2.0, Duration::days(7));
        observation.window = Some(window.into());
        observation.window_minutes = Some(10080);
        observation.model = scope.map(str::to_string);
        runtime.record_provider_limit(&observation).unwrap();
        let readiness = fixture.json(&["run", "readiness", "--json"]);
        let limit = readiness["provider_limits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|limit| limit["provider"] == provider)
            .unwrap();
        assert_eq!(limit["source"], "event");
        assert_eq!(limit["window"], window);
        assert_eq!(limit["scope"].as_str(), scope);
        assert_eq!(limit["used_percent"], 2.0);
        assert_eq!(
            limit["resets_at"]
                .as_str()
                .unwrap()
                .parse::<DateTime<Utc>>()
                .unwrap(),
            observation
                .resets_at
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
                .parse::<DateTime<Utc>>()
                .unwrap()
        );
        let pressure = readiness["provider_pressures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["provider"] == provider)
            .unwrap();
        assert_eq!(
            pressure["reports_usage"], true,
            "{provider} usage must be recognized by readiness"
        );
        let scoped = if scope.is_some() {
            readiness["crew_pressures"]
                .as_array()
                .unwrap()
                .iter()
                .find(|crew| crew["crew"] == "agy-flash")
                .unwrap()
        } else {
            pressure
        };
        assert_eq!(scoped["pressure_window"]["window"], window);
        assert_eq!(scoped["pressure_window"]["used_percent"], 2.0);
    }
}

#[test]
fn every_surface_reads_the_seeded_provider_limit_store() {
    if !isolated_run_observation(
        "run_observation::provider_limits::every_surface_reads_the_seeded_provider_limit_store",
    ) {
        return;
    }
    let fixture = Fixture::init();
    configure_crews(&fixture);
    orbit_registry::ensure_machine_identity(&fixture.home.join(".orbit"), || {
        Ok(orbit_registry::NewMachineIdentity {
            name: "provider-limits-host".into(),
            task_prefix: "PL".into(),
        })
    })
    .unwrap();
    let runtime = orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_resolved_roots(
        orbit_core::runtime::OrbitRuntimeRoots {
            global_root: fixture.home.join(".orbit"),
            shared_root: fixture.work.join(".orbit"),
            local_root: fixture.work.join(".orbit"),
        },
    )
    .unwrap();
    let reading_host = runtime.automation_machine_identity().unwrap();
    // Claude past the default 90% threshold, Codex well below it; Gemini
    // reports no usage at all.
    runtime
        .record_provider_limit(&reading("claude", 93.0, Duration::hours(2)))
        .unwrap();
    runtime
        .record_provider_limit(&reading("codex", 40.0, Duration::hours(3)))
        .unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Provider limit fixture".into(),
            description: "A task whose explicit crew's provider is at its limit.".into(),
            acceptance_criteria: vec!["Delivered.".into()],
            plan: "1. Deliver it.".into(),
            complexity: TaskComplexity::Hard,
            context_files: vec!["dir:.".into()],
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            crew: Some("opus".into()),
            ..Default::default()
        })
        .unwrap()
        .id;

    // Readiness JSON: every live reading, and the task's own wait.
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let limits = readiness["provider_limits"].as_array().unwrap();
    let claude = limits
        .iter()
        .find(|limit| limit["provider"] == "claude")
        .unwrap_or_else(|| panic!("no claude reading: {readiness:#}"));
    assert_eq!(claude["gated"], true, "{claude}");
    assert_eq!(claude["used_percent"], 93.0);
    assert_eq!(claude["threshold"], 90);
    assert_eq!(claude["window"], "five_hour");
    assert_eq!(claude["source"], "event");
    assert!(claude["resets_at"].is_string() && claude["until"].is_string());
    assert!(
        claude["crews"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("opus")),
        "{claude}"
    );
    let codex = limits
        .iter()
        .find(|limit| limit["provider"] == "codex")
        .unwrap();
    assert_eq!(codex["gated"], false, "{codex}");
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task)
        .unwrap();
    assert_eq!(entry["reason"], "provider_limit", "{readiness:#}");
    let detail = entry["detail"].as_str().unwrap();
    assert!(
        detail.contains("claude five_hour at 93%") && detail.contains("opus"),
        "{detail}"
    );

    // Readiness text: a line per gated provider, and the task's detail.
    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("Provider limits: "))
        .unwrap_or_else(|| panic!("no Provider limits line: {text}"));
    assert!(
        line.starts_with("Provider limits: claude five_hour 93% >= 90% until ")
            && line.contains("opus")
            && line.ends_with(" skipped"),
        "{line}"
    );
    assert!(
        !text.contains("Provider limits: codex"),
        "a reading below its threshold holds nothing: {text}"
    );
    let task_line = text
        .lines()
        .find(|line| line.starts_with(&format!("{task}: ")))
        .unwrap();
    assert!(
        task_line.contains("waiting (provider_limit)") && task_line.contains(detail),
        "{task_line}"
    );

    // A pull drain's crew window excludes the limited crew until its reset.
    let drain = "jrun-provider-limit-drain";
    let at = Utc::now();
    let db = fixture.db();
    db.execute(
        "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid) VALUES (?1,?2,'workspace_pull_pipeline',1,'running',?3,?3,?3,?4)",
        params![drain, fixture.workspace_id(), at.to_rfc3339(), std::process::id()],
    )
    .unwrap();
    let mut state = PipelineState::new(
        drain.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.pull_drain.pull_crew_preflight = Some(PullCrewPreflight {
        checked_at: at,
        runnable: vec!["opus".into(), "sol".into()],
        default_crew: Some("sol".into()),
        excluded: vec![],
    });
    runtime.write_run_state(drain, &state).unwrap();
    let shown = fixture.json(&["run", "show", drain, "--no-reconcile", "--json"]);
    let window = &shown["crew_window"];
    let exclusion = window["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|exclusion| exclusion["crew"] == "opus")
        .unwrap_or_else(|| panic!("opus not excluded: {window:#}"));
    assert_eq!(exclusion["source"], "provider_limit", "{exclusion}");
    assert!(
        exclusion["reason"].as_str().unwrap().contains(reading_host),
        "the JSON exclusion identifies the local reading's host: {exclusion}"
    );
    assert_eq!(exclusion["until"], claude["until"], "{exclusion}");
    assert_eq!(window["runnable"], serde_json::json!(["sol"]), "{window}");
    let text = fixture
        .orbit()
        .args(["run", "show", drain, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines().any(|line| line.contains("Crews:")
            && line.contains("excluded opus (provider_limit until ")
            && line.contains("claude five_hour at 93%")
            && line.contains(reading_host)),
        "{text}"
    );

    // Doctor: one row per configured provider, and the ungated lanes.
    // It can exit nonzero for providers whose CLI this fixture lacks.
    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let gated = doctor_row(&rows, "provider-limits:claude");
    assert_eq!(gated["status"], "warning", "{gated}");
    let message = gated["message"].as_str().unwrap();
    assert!(
        message.starts_with("claude five_hour 93% >= 90% until ") && message.contains("opus"),
        "{gated}"
    );
    let below = doctor_row(&rows, "provider-limits:codex");
    assert_eq!(below["status"], "ok", "{below}");
    assert!(
        below["message"]
            .as_str()
            .unwrap()
            .contains("five_hour 40% (limit 90%)"),
        "{below}"
    );
    let silent = doctor_row(&rows, "provider-limits:gemini");
    assert_eq!(silent["status"], "info", "{silent}");
    assert_eq!(
        silent["message"],
        "gemini: no usage signal; Orbit learns limits from failures; pressure ×1 (no live weekly usage)"
    );
    let lane = "workflow.purpose_crews.review";
    let row = doctor_row(&rows, &format!("provider-limits:{lane}"));
    assert_eq!(row["status"], "warning", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("{lane} 'opus' uses claude")) && message.contains("not gated"),
        "{row}"
    );

    // Doctor providers: the same rows under each provider's executor.
    let providers = fixture.json(&["doctor", "providers", "--json"]);
    let executor = |name: &str| {
        providers
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("no {name} executor: {providers:#}"))
            .clone()
    };
    let claude = executor("claude");
    let checks = claude["provider_limits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["check"].as_str().unwrap(),
                row["status"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        checks.contains(&("provider-limits:claude", "warning"))
            && checks.contains(&("provider-limits:workflow.purpose_crews.review", "warning")),
        "{claude}"
    );
    assert_eq!(
        executor("gemini")["provider_limits"][0]["status"],
        "info",
        "{providers:#}"
    );

    // A seven-day ledger reading with a live reset still stays neutral.
    runtime
        .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
            job_run_id: "pressure-ledger-fixture".into(),
            account: None,
            activity_id: "implement_one".into(),
            agent: "grok".into(),
            provider: Some("grok".into()),
            model: None,
            task_ids: Vec::new(),
            trace: orbit_types::telemetry::InvocationTrace {
                provider_cost_usd: Some(90.0),
                ..Default::default()
            },
        })
        .unwrap();
    // The same public snapshot adds pressure without changing the existing gate.
    let weekly = |provider, window: &str, used, remaining| {
        let mut observation = reading(provider, used, remaining);
        observation.window = Some(window.into());
        observation.window_minutes = Some(10080);
        observation
    };
    for observation in [
        weekly("claude", "seven_day", 10.0, Duration::hours(6)),
        // Deliberately not named seven_day: duration defines the weekly window.
        weekly("codex", "secondary", 0.0, Duration::days(7)),
    ] {
        runtime.record_provider_limit(&observation).unwrap();
    }
    let mut opus = weekly("claude", "seven_day_opus", 95.0, Duration::days(6));
    opus.model = Some("opus".into());
    runtime.record_provider_limit(&opus).unwrap();
    let mut credits = weekly("claude", "seven_day", 50.0, Duration::days(7));
    credits.account = Some("credits".into());
    runtime.record_provider_limit(&credits).unwrap();
    for account in ["error", "lapsed", "no-reset", "overage"] {
        let mut observation = weekly("claude", "seven_day", 90.0, Duration::hours(1));
        observation.account = Some(account.into());
        match account {
            "error" => observation.source = ProviderLimitSource::Error,
            "lapsed" => observation.resets_at = Some(Utc::now() - Duration::minutes(1)),
            "no-reset" => observation.resets_at = None,
            "overage" => observation.gating = false,
            _ => unreachable!(),
        }
        runtime.record_provider_limit(&observation).unwrap();
    }
    let readiness = fixture.json(&["run", "readiness", "--json"]);
    let account = |provider: &str, account: Option<&str>| {
        readiness["provider_pressures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["provider"] == provider && item["account"].as_str() == account)
            .unwrap()
    };
    let crew = |name: &str| {
        readiness["crew_pressures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["crew"] == name)
            .unwrap()
    };
    let near = |item: &Value, expected: f64| {
        let actual = item["pressure"].as_f64().unwrap();
        assert!(
            (actual - expected).abs() < expected.abs().max(1.0) * 0.005,
            "{expected} expected, got {item}"
        );
    };
    near(account("claude", None), 25.2);
    assert_eq!(
        account("claude", None)["pressure_window"]["window"],
        "seven_day"
    );
    near(account("codex", None), 1.0);
    assert_eq!(
        account("codex", None)["pressure_window"]["window"],
        "secondary"
    );
    near(crew("opus"), 0.05 / (6.0 / 7.0));
    assert_eq!(crew("opus")["pressure_window"]["window"], "seven_day_opus");
    near(crew("sonnet"), 25.2);
    near(crew("credits"), 0.5);
    near(account("claude", Some("credits")), 0.5);
    for name in ["gem", "ledger", "error", "lapsed", "no-reset", "overage"] {
        assert_eq!(
            crew(name)["pressure"],
            1.0,
            "neutral {name}: {}",
            crew(name)
        );
        assert!(crew(name)["pressure_window"].is_null(), "{}", crew(name));
    }
    assert!(
        readiness["provider_limits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["provider"] == "grok"
                    && item["source"] == "ledger"
                    && item["used_percent"] == 90.0
                    && item["resets_at"].is_string()
            }),
        "the neutral budget case must exercise a real ledger reading"
    );
    // Replacing only the five-hour usage cannot change weekly pressure.
    runtime
        .record_provider_limit(&reading("claude", 5.0, Duration::hours(2)))
        .unwrap();
    let changed = fixture.json(&["run", "readiness", "--json"]);
    for (old, new) in readiness["crew_pressures"]
        .as_array()
        .unwrap()
        .iter()
        .zip(changed["crew_pressures"].as_array().unwrap())
    {
        assert_eq!(old["crew"], new["crew"]);
        assert!(
            (old["pressure"].as_f64().unwrap() - new["pressure"].as_f64().unwrap()).abs() < 0.01
        );
        assert_eq!(old["pressure_window"], new["pressure_window"]);
    }
    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines()
            .any(|line| line.starts_with("Provider pressure: claude ×25.")
                && line.contains("(seven_day 10%, resets ")),
        "{text}"
    );
    assert!(
        text.contains("Crew pressure: opus ×0.06 (seven_day_opus [opus] 95%, resets "),
        "{text}"
    );
    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let row = doctor_row(&rows, "provider-limits:claude");
    assert!(
        row["message"].as_str().unwrap().contains("pressure ×25.")
            && row["message"]
                .as_str()
                .unwrap()
                .contains("(seven_day 10%, resets "),
        "{row}"
    );
    let row = doctor_row(&rows, "provider-limits:claude/credits");
    assert!(
        row["message"]
            .as_str()
            .unwrap()
            .contains("pressure ×0.5 (seven_day 50%, resets "),
        "{row}"
    );
}
