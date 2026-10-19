//! Recorded admission decisions reach both real CLI observation commands.

use super::*;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::workflow::{
    DrainAdmissionForecast, DrainAdmissionPass, DrainPassObservation, DrainPassOutcome,
    PipelineState,
};
use serde_json::json;

#[test]
fn recorded_forecast_explains_skips_shadow_headroom_and_starvation_without_writes() {
    if !isolated_run_observation(
        "run_observation::admission_forecast::recorded_forecast_explains_skips_shadow_headroom_and_starvation_without_writes",
    ) {
        return;
    }
    let fixture = Fixture::init();
    // Every readiness read here is compared with a baseline, so the CLI must
    // never sample the live host: a loaded test host would add a throttle
    // hold between the reads.
    fixture
        .orbit()
        .args([
            "config",
            "set",
            "workflow.resource_throttle.enabled",
            "false",
            "--global",
        ])
        .assert()
        .success();
    fs::OpenOptions::new()
        .append(true)
        .open(fixture.home.join(".orbit/config.toml"))
        .unwrap()
        .write_all(
            b"\n[machine.auto_concurrency]\nadmit_cpu_percent = 89\nadmit_memory_percent = 74\n",
        )
        .unwrap();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let mut ids = Vec::new();
    for title in ["heavy candidate", "small candidate", "fitting candidate"] {
        let path = format!("{}.rs", ids.len());
        fs::write(fixture.work.join(&path), "fixture").unwrap();
        ids.push(
            runtime
                .add_task(TaskAddParams {
                    title: title.into(),
                    description: "Forecast observation fixture".into(),
                    acceptance_criteria: vec!["Observable".into()],
                    plan: "Fixture plan".into(),
                    context_files: vec![format!("file:{path}")],
                    task_type: Some(TaskType::Feature),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap()
                .id,
        );
    }
    let id = "jrun-cli-forecast-observation";
    let now = chrono::Utc::now();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![id, fixture.workspace_id(), now.to_rfc3339(), std::process::id()],
    ).unwrap();
    let mut state = PipelineState::new(id.into(), "workspace_auto_pipeline".into(), json!({}));
    state.pull_drain.drain_last_pass = Some(
        serde_json::from_value::<DrainAdmissionPass>(json!({
            "recorded_at": now, "queued": 3,
        }))
        .unwrap(),
    );
    runtime.write_run_state(id, &state).unwrap();
    let baseline = fixture.json(&["run", "readiness", "--json"]);
    for task in baseline["tasks"].as_array().unwrap() {
        assert_eq!(task["reason"], "ready", "{baseline:#}");
    }
    let baseline_text = text(&fixture, &["run", "readiness"]);
    let snapshot = fixture.snapshot();

    for mode in ["enforce", "shadow"] {
        let block = json!({
            "mode": mode, "forecast_at": now,
            "cpu_peak": {"minute": 6, "value": 7.2, "percent": 90.0},
            "memory_peak": {"minute": 2, "value": 8.0 * 1024.0 * 1024.0 * 1024.0, "percent": 40.0},
            "admitted": [{"task_id": ids[2], "profile_source": "without_complexity"}],
            "skipped": [
                {"task_id": ids[0], "profile_source": "exact", "resource": "cpu", "minute": 6, "percent": 94.0, "detail": "cpu 94% at +6m"},
                {"task_id": ids[1], "profile_source": "heaviest_on_host", "detail": format!("starvation guard protects {}", ids[0])},
            ],
            "head": {
                "task_id": ids[0], "failing_since": now, "protected": true,
                "miss": {"profile_source": "exact", "profile_key": "change/medium", "resource": "cpu", "minute": 6, "percent": 94.0, "baseline_percent": 12.0},
                "stall": {
                    "reference": {"profile_source": "exact", "profile_key": "change/medium", "resource": "cpu", "minute": 6, "percent": 94.0, "baseline_percent": 12.0},
                    "since": now, "passes": 3, "stuck": true,
                },
            },
        });
        let forecast = serde_json::from_value::<DrainAdmissionForecast>(block).unwrap();
        let mut pass = state.pull_drain.drain_last_pass.clone().unwrap();
        pass.admission_forecast = Some(forecast.clone());
        state.pull_drain.record_admission_pass(pass);
        runtime.write_run_state(id, &state).unwrap();
        let before = fixture.snapshot();
        let before_state = json!(runtime.read_run_state(id).unwrap());
        let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
        assert_eq!(
            shown["pipeline_state"]["drain_last_pass"]["admission_forecast"],
            json!(forecast)
        );
        assert_eq!(
            shown["pipeline_state"]["drain_last_forecast"],
            json!(forecast)
        );
        let show_text = text(&fixture, &["run", "show", id, "--no-reconcile"]);
        let line = show_text
            .lines()
            .find(|line| line.starts_with("Admission:"))
            .unwrap();
        // Each decision names its task by ID and quoted title, so one title
        // does not run on into the next clause.
        for named in [
            format!("admitted {} \"fitting candidate\"", ids[2]),
            format!("skipped {} \"heavy candidate\" (cpu 94% at +6m)", ids[0]),
            format!("starvation guard protects {} \"heavy candidate\"", ids[0]),
        ] {
            assert!(line.contains(&named), "{named}: {line}");
        }
        assert_eq!(line.contains("shadow"), mode == "shadow", "{line}");
        let ready_text = text(&fixture, &["run", "readiness"]);
        for rendered in [&show_text, &ready_text] {
            let stuck = rendered
                .lines()
                .find(|line| line.starts_with("Stuck head"))
                .unwrap_or_else(|| panic!("stuck head not shown: {rendered}"));
            for named in [
                format!("{} \"heavy candidate\"", ids[0]).as_str(),
                "cpu 94% at +6m",
                "change/medium via exact",
                "cpu baseline 12%",
                "3 protected passes",
            ] {
                assert!(stuck.contains(named), "{named}: {stuck}");
            }
            assert_eq!(stuck.contains("shadow"), mode == "shadow", "{stuck}");
        }

        let readiness = fixture.json(&["run", "readiness", "--json"]);
        let view = &readiness["capacity"]["admission_forecast"];
        assert_eq!(
            view["cpu_peak"],
            shown["pipeline_state"]["drain_last_pass"]["admission_forecast"]["cpu_peak"]
        );
        assert_eq!(view["headroom"]["cpu_percent"], -1.0);
        assert_eq!(view["headroom"]["memory_percent"], 34.0);
        let tasks = readiness["tasks"].as_array().unwrap();
        let heavy = tasks.iter().find(|task| task["task_id"] == ids[0]).unwrap();
        assert_eq!(heavy["admission_forecast"]["profile_source"], "exact");
        assert_eq!(heavy["admission_forecast"]["minute"], 6);
        assert_eq!(
            heavy["reason"],
            if mode == "shadow" { "ready" } else { "gap_fit" }
        );
        assert_eq!(heavy["eligible"], mode == "shadow");
        let small = tasks.iter().find(|task| task["task_id"] == ids[1]).unwrap();
        assert_eq!(
            small["admission_forecast"]["profile_source"],
            "heaviest_on_host"
        );
        let fitting = tasks.iter().find(|task| task["task_id"] == ids[2]).unwrap();
        assert_eq!(fitting["reason"], "ready");
        assert_eq!(
            fitting["admission_forecast"]["profile_source"],
            "without_complexity"
        );
        let limited = fixture.json(&["run", "readiness", "--concurrency", "1", "--json"]);
        assert!(
            limited["tasks"].as_array().unwrap().iter().any(|task| {
                task["reason"] == "capacity_saturated"
                    && task["eligible"] == false
                    && task["admission_forecast"]["profile_source"].is_string()
            }),
            "live capacity hold lost: {limited:#}"
        );
        let ready_text = text(&fixture, &["run", "readiness"]);
        assert!(
            ready_text.contains("cpu 7.2 cores (90.0%) at +6m; headroom: -1.0"),
            "{ready_text}"
        );
        assert!(
            ready_text.contains("Forecast peak: memory 8.0 GiB (40.0%) at +2m; headroom: 34.0"),
            "{ready_text}"
        );
        assert!(!ready_text.contains("Last forecast"), "{ready_text}");
        assert!(
            ready_text.contains("profile-source=heaviest_on_host"),
            "{ready_text}"
        );
        assert!(ready_text.contains("cpu 94% at +6m"), "{ready_text}");
        assert_eq!(json!(runtime.read_run_state(id).unwrap()), before_state);
        assert_eq!(
            fixture
                .db()
                .query_row("SELECT COUNT(*) FROM admission_reservations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            fixture.snapshot(),
            before,
            "observation mutated runs or reservations"
        );
    }

    // Idle passes must preserve the last built forecast through the same
    // state transition the drain uses, without turning old skips into holds.
    let mut retained = state.pull_drain.drain_last_forecast.clone().unwrap();
    retained.mode = "enforce".into();
    let mut built = state.pull_drain.drain_last_pass.clone().unwrap();
    built.admission_forecast = Some(retained.clone());
    state.pull_drain.record_admission_pass(built);
    for with_head in [false, true, true] {
        let mut idle = state.pull_drain.drain_last_pass.clone().unwrap();
        idle.recorded_at = now + chrono::Duration::minutes(1);
        idle.admission_forecast = if with_head {
            let mut forecast = retained.clone();
            forecast.forecast_at = None;
            forecast.cpu_peak = None;
            forecast.memory_peak = None;
            forecast.admitted.clear();
            forecast.skipped.clear();
            Some(forecast)
        } else {
            None
        };
        state.pull_drain.record_admission_pass(idle);
        runtime.write_run_state(id, &state).unwrap();
        let before_state = json!(runtime.read_run_state(id).unwrap());
        let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
        assert_eq!(
            shown["pipeline_state"]["drain_last_forecast"],
            json!(retained)
        );
        assert!(
            shown["pipeline_state"]["drain_last_pass"]["admission_forecast"]["forecast_at"]
                .is_null()
        );
        let readiness = fixture.json(&["run", "readiness", "--json"]);
        let last = &readiness["capacity"]["last_admission_forecast"];
        for key in [
            "forecast_at",
            "cpu_peak",
            "memory_peak",
            "admitted",
            "skipped",
            "head",
        ] {
            assert_eq!(last[key], json!(retained)[key], "{key}");
        }
        assert_eq!(last["drain_run_id"], id);
        assert_eq!(last["drain_live"], true);
        assert!(readiness["capacity"]["admission_forecast"]["forecast_at"].is_null());
        for task in readiness["tasks"].as_array().unwrap() {
            assert_eq!(
                task["reason"], "ready",
                "historical skips must not hold tasks"
            );
            assert!(task["admission_forecast"].is_null());
        }
        for args in [
            vec!["run", "readiness"],
            vec!["run", "show", id, "--no-reconcile"],
        ] {
            let rendered = text(&fixture, &args);
            assert!(
                rendered.contains("no forecast built this pass"),
                "{rendered}"
            );
            // The retained forecast names its time, age and drain, and its
            // peaks are not presented as this pass's.
            let at = last["forecast_at"].as_str().unwrap();
            let retained_line = rendered
                .lines()
                .find(|line| line.starts_with(&format!("Last forecast {at} (")))
                .unwrap_or_else(|| panic!("retained forecast unlabelled: {rendered}"));
            assert!(
                retained_line.contains(&format!(" ago, from drain {id}): ")),
                "{retained_line}"
            );
            for named in [
                format!("admitted {} \"fitting candidate\"", ids[2]),
                format!("skipped {} \"heavy candidate\" (cpu 94% at +6m)", ids[0]),
            ] {
                assert!(retained_line.contains(&named), "{named}: {retained_line}");
            }
            assert!(
                rendered.contains("Last forecast peak: cpu 7.2 cores (90.0%) at +6m"),
                "{rendered}"
            );
            assert!(
                rendered.contains("Last forecast peak: memory 8.0 GiB (40.0%) at +2m"),
                "{rendered}"
            );
            assert!(!rendered.contains("Forecast peak:"), "{rendered}");
        }
        assert_eq!(json!(runtime.read_run_state(id).unwrap()), before_state);
        assert_eq!(fixture.snapshot(), snapshot);
    }

    // Without retained history a protected head is still observed on its own.
    state.pull_drain.drain_last_forecast = None;
    let forecast = state
        .pull_drain
        .drain_last_pass
        .as_mut()
        .unwrap()
        .admission_forecast
        .as_mut()
        .unwrap();
    forecast.forecast_at = None;
    forecast.cpu_peak = None;
    forecast.memory_peak = None;
    forecast.admitted.clear();
    forecast.skipped.clear();
    runtime.write_run_state(id, &state).unwrap();
    let shown = text(&fixture, &["run", "show", id, "--no-reconcile"]);
    let head = format!("{} \"heavy candidate\"", ids[0]);
    assert!(
        shown.contains(&format!("starvation guard protects {head}")),
        "{shown}"
    );
    assert!(shown.contains(&format!("Stuck head: {head}")), "{shown}");
    let ready = text(&fixture, &["run", "readiness"]);
    assert!(
        ready.contains(&format!("starvation guard protects {head}")),
        "{ready}"
    );
    assert!(ready.contains(&format!("Stuck head: {head}")), "{ready}");
    assert!(!ready.contains("Forecast peak"), "{ready}");

    // Unknown host size must not be formatted as zero headroom.
    let forecast = state
        .pull_drain
        .drain_last_pass
        .as_mut()
        .unwrap()
        .admission_forecast
        .as_mut()
        .unwrap();
    forecast.forecast_at = Some(now);
    forecast.cpu_peak = Some(orbit_types::workflow::ForecastPeak {
        minute: 0,
        value: 1.5,
        percent: None,
    });
    runtime.write_run_state(id, &state).unwrap();
    let ready = fixture.json(&["run", "readiness", "--json"]);
    assert!(ready["capacity"]["admission_forecast"]["headroom"]["cpu_percent"].is_null());
    let ready = text(&fixture, &["run", "readiness"]);
    assert!(
        ready.contains("host capacity unknown") && ready.contains("headroom: unknown"),
        "{ready}"
    );

    // The last terminal drain remains useful history, not a current hold.
    let forecast = state
        .pull_drain
        .drain_last_pass
        .as_mut()
        .unwrap()
        .admission_forecast
        .as_mut()
        .unwrap();
    forecast.mode = "enforce".into();
    forecast.skipped.push(orbit_types::workflow::GapFitSkip {
        task_id: ids[0].clone(),
        profile_source: "exact".into(),
        resource: Some("cpu".into()),
        minute: Some(6),
        percent: Some(94.0),
        detail: "cpu 94% at +6m".into(),
    });
    runtime.write_run_state(id, &state).unwrap();
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state = 'success', finished_at = started_at, duration_ms = 0 WHERE run_id = ?1",
            [id],
        )
        .unwrap();
    let history = fixture.json(&["run", "readiness", "--json"]);
    for task in history["tasks"].as_array().unwrap() {
        assert_eq!(
            task["reason"], "ready",
            "historical skip became a current hold: {history:#}"
        );
    }
    assert_eq!(
        history["capacity"]["admission_forecast"]["skipped"][0]["percent"],
        94.0
    );
    assert_eq!(
        history["capacity"]["admission_forecast"]["drain_run_id"],
        id
    );
    assert_eq!(
        history["capacity"]["admission_forecast"]["drain_live"],
        false
    );
    // A finished drain's last pass is history: never an unlabelled current
    // peak on an idle host.
    for (args, idle) in [
        (vec!["run", "readiness"], "; no drain is live): "),
        (vec!["run", "show", id, "--no-reconcile"], "): "),
    ] {
        let rendered = text(&fixture, &args);
        let line = rendered
            .lines()
            .find(|line| line.starts_with("Last forecast "))
            .unwrap_or_else(|| panic!("finished drain's forecast unlabelled: {rendered}"));
        assert!(
            line.contains(&format!(" ago, from drain {id}, which has ended{idle}")),
            "{line}"
        );
        assert!(!rendered.contains("Admission: forecast"), "{rendered}");
        assert!(
            rendered.contains("Last forecast peak: cpu 1.5 cores (host capacity unknown)"),
            "{rendered}"
        );
        assert!(!rendered.contains("Forecast peak:"), "{rendered}");
    }
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state = 'running', finished_at = NULL, duration_ms = NULL WHERE run_id = ?1",
            [id],
        )
        .unwrap();

    state
        .pull_drain
        .drain_last_pass
        .as_mut()
        .unwrap()
        .admission_forecast = None;
    runtime.write_run_state(id, &state).unwrap();
    let absent = fixture.json(&["run", "readiness", "--json"]);
    assert_eq!(absent, baseline, "absent forecast changed readiness JSON");
    assert_eq!(text(&fixture, &["run", "readiness"]), baseline_text);
    let shown = text(&fixture, &["run", "show", id, "--no-reconcile"]);
    assert!(
        !shown.contains("Admission:") && !shown.contains("Stuck head"),
        "{shown}"
    );
    assert_eq!(fixture.snapshot(), snapshot);
}

/// A drain's cumulative pass counters reach `run show` (JSON and the text
/// Auto-concurrency section) and the per-drain limiter shares of
/// `run profiles --accuracy` (JSON and table) without any write.
#[test]
fn drain_pass_counters_reach_run_show_and_accuracy_limiters_without_writes() {
    if !isolated_run_observation(
        "run_observation::admission_forecast::drain_pass_counters_reach_run_show_and_accuracy_limiters_without_writes",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let id = "jrun-cli-drain-pass-counters";
    let workspace_id = fixture.workspace_id();
    let now = chrono::Utc::now();
    fixture.db().execute(
        "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'workspace_auto_pipeline',1,'running','{}',?3,?3,?3,?4)",
        params![id, workspace_id, now.to_rfc3339(), std::process::id()],
    ).unwrap();
    let mut state = PipelineState::new(id.into(), "workspace_auto_pipeline".into(), json!({}));
    let waiting = |reason: &str| [reason.to_string()].into_iter().collect();
    for observation in [
        DrainPassObservation {
            outcome: DrainPassOutcome::CeilingFull,
            waiting_reasons: waiting("context_lock_conflict"),
            ..Default::default()
        },
        DrainPassObservation {
            outcome: DrainPassOutcome::CeilingFull,
            ..Default::default()
        },
        DrainPassObservation {
            outcome: DrainPassOutcome::GapFitHeld,
            waiting_reasons: waiting("gap_fit"),
            ..Default::default()
        },
        DrainPassObservation {
            outcome: DrainPassOutcome::Throttle,
            throttled: waiting("memory"),
            ..Default::default()
        },
    ] {
        state.pull_drain.count_admission_pass(&observation, now);
    }
    let counters = state.pull_drain.drain_pass_counters.clone().unwrap();
    runtime.write_run_state(id, &state).unwrap();
    let snapshot = fixture.snapshot();

    let shown = fixture.json(&["run", "show", id, "--no-reconcile", "--json"]);
    assert_eq!(
        shown["pipeline_state"]["drain_pass_counters"],
        json!(counters),
        "{shown:#}"
    );
    assert_eq!(
        shown["pipeline_state"]["drain_pass_counters"]["outcomes"],
        json!({"ceiling_full": 2, "gap_fit_held": 1, "throttle": 1})
    );
    let show_text = text(&fixture, &["run", "show", id, "--no-reconcile"]);
    let line = show_text
        .lines()
        .find(|line| line.starts_with("Admission passes:"))
        .unwrap_or_else(|| panic!("no counter line: {show_text}"));
    for named in [
        "4 passes since",
        "ceiling_full 2 (50%)",
        "gap_fit_held 1 (25%)",
        "throttle 1 (25%)",
        "throttled by memory 1",
        "context_lock_conflict in 1 (25%)",
    ] {
        assert!(line.contains(named), "{named}: {line}");
    }

    let report = fixture.json(&["run", "profiles", "--accuracy", "--json"]);
    let limiters = report["limiters"].as_array().unwrap();
    let drain = limiters
        .iter()
        .find(|drain| drain["run_id"] == id)
        .unwrap_or_else(|| panic!("drain missing from limiters: {report:#}"));
    assert_eq!(drain["workspace_id"], workspace_id.as_str());
    assert_eq!(drain["counters"], json!(counters));
    assert_eq!(drain["shares"]["ceiling_full"], 0.5);
    assert_eq!(drain["shares"]["gap_fit_held"], 0.25);
    assert_eq!(drain["shares"]["throttle"], 0.25);
    assert_eq!(drain["shares"]["admitted"], 0.0);
    let total: f64 = drain["shares"]
        .as_object()
        .unwrap()
        .values()
        .map(|share| share.as_f64().unwrap())
        .sum();
    assert_eq!(total, 1.0, "outcome shares must sum to one: {drain:#}");
    assert_eq!(drain["waiting_shares"]["gap_fit"], 0.25);

    let plain = text(
        &fixture,
        &["run", "profiles", "--accuracy", "--format", "plain"],
    );
    let row: Vec<&str> = plain
        .lines()
        .find(|line| line.starts_with(&format!("{workspace_id}\t{id}\t")))
        .unwrap_or_else(|| panic!("no limiter row: {plain}"))
        .split('\t')
        .collect();
    assert_eq!(
        row[2..11],
        [
            "4", "0% (0)", "0% (0)", "0% (0)", "25% (1)", "0% (0)", "50% (2)", "25% (1)", "0% (0)"
        ],
        "{plain}"
    );
    assert_eq!(
        row[11],
        "throttled by memory 1; waiting: context_lock_conflict in 1 (25%), gap_fit in 1 (25%)"
    );

    // A drain with no selected forecast is not in a mode-selected report.
    let enforce = fixture.json(&[
        "run",
        "profiles",
        "--accuracy",
        "--mode",
        "enforce",
        "--json",
    ]);
    assert_eq!(enforce["limiters"], json!([]), "{enforce:#}");
    assert_eq!(fixture.snapshot(), snapshot, "observation mutated state");
}

fn text(fixture: &Fixture, args: &[&str]) -> String {
    let output = fixture.orbit().args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}
