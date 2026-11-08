//! Explicit repilot commands submit one pending run through the operator CLI.

use crate::isolated_cli_fixture::Fixture;

#[test]
fn repilot_and_update_flag_submit_once_and_refuse_pending_duplicates() {
    if crate::workflow_gate::wait_if_requested() {
        return;
    }
    if crate::isolated_test::run_in_child(
        "repilot_cli::repilot_and_update_flag_submit_once_and_refuse_pending_duplicates",
    ) {
        return;
    }
    let fixture = Fixture::new();
    crate::fixture_crew::configure_sol(&fixture.root);
    let gate = crate::workflow_gate::WorkflowGate::new();
    let jobs = fixture.root.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let job = serde_json::json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pilot_pipeline"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [gate.step("hold")]}});
    std::fs::write(jobs.join("task_pilot_pipeline.yaml"), job.to_string()).unwrap();
    for update in [false, true] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            "Explicit repilot fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Inspect the requirements",
            "--json",
        ]);
        let id = task["id"].as_str().unwrap();
        let args = if update {
            vec![
                "task",
                "update",
                id,
                "--description",
                "Edited requirements",
                "--repilot",
                "--json",
            ]
        } else {
            vec!["task", "repilot", id, "--json"]
        };
        let submitted = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .clone();
        if !update {
            // The shared submission result names the task and keeps the
            // published `job_name` and `queue_position` [ORB-22908].
            let submitted: serde_json::Value = serde_json::from_slice(&submitted.stdout).unwrap();
            assert_eq!(submitted["task_id"], id, "{submitted}");
            assert_eq!(submitted["job_name"], "task_pilot_pipeline", "{submitted}");
            assert_eq!(submitted["queued"], false, "{submitted}");
            assert!(submitted.get("queue_position").is_some(), "{submitted}");
        }
        gate.wait_ready();
        let duplicate = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .failure()
            .get_output()
            .clone();
        assert!(
            String::from_utf8_lossy(&duplicate.stderr).contains("already held by pilot run"),
            "{duplicate:?}"
        );
        if !update {
            // The refusal keeps its stable code instead of `invalid_input`.
            let refused: serde_json::Value = serde_json::from_slice(&duplicate.stderr).unwrap();
            assert_eq!(refused["code"], "task_pilot_already_running", "{refused}");
        }
        let shown = fixture.json(&["task", "show", id, "--json"]);
        assert_eq!(shown["status"], "proposed");
        if update {
            assert_eq!(shown["description"], "Edited requirements");
        }
        gate.release();
    }
    // A missing task keeps its not-found kind instead of `invalid_input`.
    let missing = fixture
        .command(&["task", "repilot", "ORB-99999", "--json"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let missing: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(missing["code"], "task_not_found", "{missing}");
    let runs = fixture.json(&[
        "run",
        "history",
        "-j",
        "task_pilot_pipeline",
        "--no-reconcile",
        "--json",
    ]);
    assert_eq!(runs["runs"].as_array().unwrap().len(), 2);
}

/// `repilot --full` asks the run for a full assessment, so prepare never
/// re-checks the held one; a plain repilot leaves the choice to prepare
/// [ORB-21124].
#[test]
fn repilot_full_requests_a_full_assessment() {
    if crate::workflow_gate::wait_if_requested() {
        return;
    }
    if crate::isolated_test::run_in_child("repilot_cli::repilot_full_requests_a_full_assessment") {
        return;
    }
    let fixture = Fixture::new();
    crate::fixture_crew::configure_sol(&fixture.root);
    let gate = crate::workflow_gate::WorkflowGate::new();
    let jobs = fixture.root.join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    let job = serde_json::json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "task_pilot_pipeline"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [gate.step("hold")]}});
    std::fs::write(jobs.join("task_pilot_pipeline.yaml"), job.to_string()).unwrap();
    for full in [true, false] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            "Full repilot fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Inspect the requirements",
            "--json",
        ]);
        let id = task["id"].as_str().unwrap();
        let mut args = vec!["task", "repilot", id, "--json"];
        if full {
            args.insert(3, "--full");
        }
        let output = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .assert()
            .success()
            .get_output()
            .clone();
        let submitted: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let run_id = submitted["run_id"].as_str().expect("submitted run id");
        gate.wait_ready();
        let shown = fixture.json(&["run", "show", run_id, "--json"]);
        let input = &shown["run"]["input"];
        assert_eq!(input["task_ids"], serde_json::json!([id]), "{shown}");
        assert_eq!(
            input.get("full_pilot").and_then(serde_json::Value::as_bool),
            full.then_some(true),
            "{shown}"
        );
        gate.release();
    }
}
