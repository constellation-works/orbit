//! [ORB-14698] One seeded provider-limit store, read through every CLI
//! surface: `run readiness` text and JSON, a pull drain's `run show` crew
//! window, and the `doctor` and `doctor providers` provider-limits rows.
use super::*;
use chrono::{Duration, Utc};
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
    ] {
        config["crews"][name]["enabled"] = value(true);
        config["crews"][name]["provider"] = value(provider);
        config["crews"][name]["model"] = value(model);
        config["crews"][name]["backend"] = value("cli");
    }
    config["workflow"]["system_crew"] = value("opus");
    config["operation"]["review_crew"] = value("opus");
    fs::write(&path, config.to_string()).unwrap();
}

fn reading(provider: &str, used_percent: f64, resets_in: Duration) -> ProviderLimitObservation {
    ProviderLimitObservation {
        provider: provider.into(),
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
fn every_surface_reads_the_seeded_provider_limit_store() {
    if !isolated_run_observation(
        "run_observation::provider_limits::every_surface_reads_the_seeded_provider_limit_store",
    ) {
        return;
    }
    let fixture = Fixture::init();
    configure_crews(&fixture);
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
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
    db.execute_batch("CREATE TABLE IF NOT EXISTS local_pull_admissions (workspace_id TEXT NOT NULL, owner_machine TEXT NOT NULL, owner_workspace TEXT NOT NULL, execution_machine TEXT NOT NULL, request_id TEXT NOT NULL, claim_id TEXT, leaf_run_id TEXT, record_json TEXT NOT NULL, PRIMARY KEY(workspace_id,owner_machine,owner_workspace,execution_machine,request_id), UNIQUE(workspace_id,owner_machine,owner_workspace,claim_id), UNIQUE(workspace_id,leaf_run_id));").unwrap();
    let mut state = PipelineState::new(
        drain.into(),
        "workspace_pull_pipeline".into(),
        serde_json::json!({}),
    );
    state.pull_crew_preflight = Some(PullCrewPreflight {
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
            && line.contains("claude five_hour at 93%")),
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
        "gemini: no usage signal; Orbit learns limits from failures"
    );
    for lane in ["workflow.system_crew", "operation.review_crew"] {
        let row = doctor_row(&rows, &format!("provider-limits:{lane}"));
        assert_eq!(row["status"], "warning", "{row}");
        let message = row["message"].as_str().unwrap();
        assert!(
            message.starts_with(&format!("{lane} 'opus' uses claude"))
                && message.contains("not gated"),
            "{row}"
        );
    }

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
            && checks.contains(&("provider-limits:workflow.system_crew", "warning")),
        "{claude}"
    );
    assert_eq!(
        executor("gemini")["provider_limits"][0]["status"],
        "info",
        "{providers:#}"
    );
}
