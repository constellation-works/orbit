//! Admission capacity and durable wait metrics through the built CLI.
use super::*;
use orbit_types::workflow::PipelineState;

#[test]
fn readiness_and_doctor_warn_for_local_and_pull_drain_capacity() {
    if !isolated_run_observation(
        "run_observation::build_budget::readiness_and_doctor_warn_for_local_and_pull_drain_capacity",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let runtime =
        OrbitRuntime::from_roots(&fixture.home.join(".orbit"), &fixture.work.join(".orbit"))
            .unwrap();
    let budget = fixture.home.join(".orbit/cache/build-budget");
    fs::create_dir_all(&budget).unwrap();
    fs::write(budget.join("slots"), "2\n").unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    for (run, job) in [
        ("jrun-budget-local", "workspace_auto_pipeline"),
        ("jrun-budget-pull", "workspace_pull_pipeline"),
    ] {
        fixture.db().execute("INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at, started_at, created_at, pid, input_json) VALUES (?1,?2,?3,1,'running',?4,?4,?4,?5,?6)", params![run,fixture.workspace_id(),job,now,std::process::id(),r#"{"max_active_leaf_runs":8}"#]).unwrap();
        let mut state = PipelineState::new(
            run.into(),
            job.into(),
            serde_json::json!({"max_active_leaf_runs":8}),
        );
        state.set_drain_worker_limit(3, 8, "fixture".into(), None, None);
        runtime.write_run_state(run, &state).unwrap();
    }
    let snapshot = fixture.json(&["run", "readiness", "--json"]);
    let warnings = snapshot["capacity"]["build_budget_warnings"]
        .as_array()
        .unwrap();
    assert_eq!(warnings.len(), 2, "{snapshot}");
    for warning in warnings {
        assert_eq!(warning["concurrency"], 3, "effective resized ceiling wins");
        assert_eq!(warning["build_slots"], 2);
        assert_eq!(
            warning["settings_file"],
            budget.join("slots").display().to_string()
        );
    }
    let text = fixture.orbit().args(["run", "readiness"]).output().unwrap();
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8_lossy(&text.stdout);
    for value in [
        "3",
        "2",
        "ORBIT_BUILD_SLOTS",
        budget.join("slots").to_str().unwrap(),
    ] {
        assert!(text.contains(value), "operator capacity and remedy: {text}");
    }
    let doctor = fixture.orbit().args(["doctor", "--json"]).output().unwrap();
    let rows: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let warnings = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["check"] == "build-budget")
        .collect::<Vec<_>>();
    assert_eq!(warnings.len(), 2);
    for warning in warnings {
        assert_eq!(warning["status"], "warning");
        assert!(warning["message"].as_str().unwrap().contains("3"));
        assert!(warning["message"].as_str().unwrap().contains("2"));
        assert!(
            warning["remediation"]
                .as_str()
                .unwrap()
                .contains("ORBIT_BUILD_SLOTS")
        );
        assert!(
            warning["remediation"]
                .as_str()
                .unwrap()
                .contains(budget.join("slots").to_str().unwrap())
        );
    }
    // Explicit slot settings beat the host file; equality and bypass do not warn.
    for (name, value) in [("ORBIT_BUILD_SLOTS", "3"), ("ORBIT_BUILD_BUDGET", "0")] {
        let output = fixture
            .orbit()
            .env(name, value)
            .args(["run", "readiness", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            snapshot["capacity"]["build_budget_warnings"],
            serde_json::json!([])
        );
    }
}

#[test]
fn leaf_show_retains_failed_attempt_and_live_wait_statistics() {
    if !isolated_run_observation(
        "run_observation::build_budget::leaf_show_retains_failed_attempt_and_live_wait_statistics",
    ) {
        return;
    }
    let fixture = Fixture::init();
    let workspace = fixture.workspace_id();
    let run = "jrun-budget-leaf";
    let now = chrono::Utc::now().to_rfc3339();
    let db = fixture.db();
    db.execute("INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,scheduled_at,started_at,finished_at,created_at) VALUES (?1,?2,'task_claimed_pr_pipeline',1,'failed',?3,?3,?3,?3)",params![run,workspace,now]).unwrap();
    let events = [
        serde_json::json!({"body_kind":"step_started", "step_id":"implement_one"}),
        serde_json::json!({"body_kind":"cli_invocation_process", "provider":"claude", "pid":999999}),
        serde_json::json!({"body_kind":"cli_invocation_build_budget", "provider":"claude", "count":1,"total_ms":3000,"longest_ms":3000,"queued_wall_ms":3000,"deadline_extension_ms":3000}),
        serde_json::json!({"body_kind":"cli_invocation_build_budget", "provider":"claude", "count":2,"total_ms":5500,"longest_ms":3500,"queued_wall_ms":5000,"deadline_extension_ms":4000}),
        serde_json::json!({"body_kind":"cli_invocation_finished", "provider":"claude", "timed_out":true,"duration_ms":8000}),
    ];
    for (index, mut event) in events.into_iter().enumerate() {
        let id = format!("budget-event-{index}");
        event["event_id"] = serde_json::json!(id);
        event["ts"] = serde_json::json!(now);
        event["step_id"] = serde_json::json!("implement_one");
        db.execute("INSERT INTO v2_audit_events (workspace_id,event_id,source,schema_version,event_type,ts,run_id,agent_identity,payload_json) VALUES (?1,?2,'v2_envelope',1,'activity.progress',?3,?4,'fixture',?5)",params![workspace,id,now,run,event.to_string()]).unwrap();
    }
    let shown = fixture.json(&["run", "show", run, "--no-reconcile", "--json"]);
    let process = &shown["provider_processes"][0];
    assert_eq!(process["timed_out"], true);
    let waits = &process["build_budget_waits"];
    assert_eq!(
        waits["count"], 2,
        "cumulative snapshots do not double count: {shown}"
    );
    assert_eq!(waits["total_ms"], 5500);
    assert_eq!(waits["longest_ms"], 3500);
    assert_eq!(waits["queued_wall_ms"], 5000);
    assert_eq!(waits["deadline_extension_ms"], 4000);
    let output = fixture
        .orbit()
        .args(["run", "show", run, "--no-reconcile"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8_lossy(&output.stdout);
    for value in ["count 2", "total 5.500s", "longest 3.500s"] {
        assert!(text.contains(value), "leaf wait diagnostics: {text}");
    }
}
