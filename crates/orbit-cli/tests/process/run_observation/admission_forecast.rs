//! Recorded admission decisions reach both real CLI observation commands.

use super::*;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use orbit_types::workflow::{DrainAdmissionForecast, DrainAdmissionPass, PipelineState};
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
            "memory_peak": {"minute": 2, "value": 1024.0, "percent": 40.0},
            "admitted": [{"task_id": ids[2], "profile_source": "without_complexity"}],
            "skipped": [
                {"task_id": ids[0], "profile_source": "exact", "resource": "cpu", "minute": 6, "percent": 94.0, "detail": "cpu 94% at +6m"},
                {"task_id": ids[1], "profile_source": "heaviest_on_host", "detail": format!("starvation guard protects {}", ids[0])},
            ],
            "head": {"task_id": ids[0], "failing_since": now, "protected": true},
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
        assert!(
            line.contains("heavy candidate") && line.contains("cpu 94% at +6m"),
            "{line}"
        );
        assert!(
            line.contains("starvation guard protects heavy candidate"),
            "{line}"
        );
        assert_eq!(line.contains("shadow"), mode == "shadow", "{line}");
        for task_id in &ids {
            assert!(!line.contains(task_id), "internal ID leaked: {line}");
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
            ready_text.contains("memory 1024.0 bytes (40.0%) at +2m; headroom: 34.0"),
            "{ready_text}"
        );
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
            assert!(
                rendered.contains(last["forecast_at"].as_str().unwrap()),
                "{rendered}"
            );
            assert!(
                rendered.contains("admitted fitting candidate"),
                "{rendered}"
            );
            assert!(
                rendered.contains("skipped heavy candidate (cpu 94% at +6m)"),
                "{rendered}"
            );
            assert!(
                rendered.contains("cpu 7.2 cores (90.0%) at +6m"),
                "{rendered}"
            );
            assert!(
                rendered.contains("memory 1024.0 bytes (40.0%) at +2m"),
                "{rendered}"
            );
            let forecast_lines = rendered
                .lines()
                .filter(|line| {
                    line.starts_with("Admission:")
                        || line.starts_with("Last forecast ")
                        || line.starts_with("Forecast peak:")
                })
                .collect::<Vec<_>>()
                .join("\n");
            for task_id in &ids {
                assert!(
                    !forecast_lines.contains(task_id),
                    "internal ID leaked in forecast: {forecast_lines}"
                );
            }
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
    assert!(
        shown.contains("starvation guard protects heavy candidate"),
        "{shown}"
    );
    let ready = text(&fixture, &["run", "readiness"]);
    assert!(
        ready.contains("starvation guard protects heavy candidate"),
        "{ready}"
    );
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
            "UPDATE job_runs SET state = 'success' WHERE run_id = ?1",
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
    fixture
        .db()
        .execute(
            "UPDATE job_runs SET state = 'running' WHERE run_id = ?1",
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
    assert!(!text(&fixture, &["run", "show", id, "--no-reconcile"]).contains("Admission:"));
    assert_eq!(fixture.snapshot(), snapshot);
}

fn text(fixture: &Fixture, args: &[&str]) -> String {
    let output = fixture.orbit().args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}
